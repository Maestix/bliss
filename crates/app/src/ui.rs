//! The window.
//!
//! Screen 1c from the mockups, wired to the engine, plus the connections table
//! that xray-core cannot supply. The system frame is off: the title bar here is
//! ours, as the design has it, which means the drag region and the three window
//! buttons are ours to implement too.
//!
//! CSS stays inside the subset both renderers handle (see
//! `docs/spike-1.0-renderer.md`): no `color-mix`, no `text-overflow`, and long
//! text trimmed in code. Today that costs nothing and it keeps the Blitz door
//! open.
//!
//! **Two rules about inline styles, both paid for in bugs.** Dioxus does not
//! simply write the `style` attribute: it records the properties the node
//! already has inline, writes the new string, and then puts back every property
//! the new string did not mention (`dioxus-interpreter-js`,
//! `setAttributeInner`, case `"style"`). Two consequences:
//!
//! 1. **A property that varies by state must never simply disappear** from the
//!    string, or it is restored and sticks forever. State belongs in a class.
//!    This is why the rail kept its accent bar on every item ever clicked.
//! 2. **No `var()` inside a shorthand whose longhands cannot resolve.** Such a
//!    value is a pending substitution, the longhands read back empty, and the
//!    restore loop writes those empties over the shorthand — destroying it.
//!    Applied repeatedly it alternates: this is the rounding that flickered
//!    between round and square on the rail.
//!
//! Which properties that second rule covers was measured rather than guessed,
//! by applying each one four times through a replica of the renderer's own
//! function. Unstable: `border-radius`, `background`, `border-right` and its
//! siblings. Stable: `background-color`, `box-shadow`, `color`, `font-family`
//! — and, unexpectedly, the `font:` shorthand itself, so splitting that one up
//! fixed nothing and is kept only for being clearer.
//!
//! Both of these only bite a style string that actually changes between
//! renders; a constant one is never rewritten. That is not something to lean
//! on, though — a later edit that interpolates one value into a previously
//! constant string would bring the bug back with no warning. So none of the
//! inline styles here carry an unstable shorthand, whether they change or not,
//! and `radius_literals_match_the_design_tokens` holds the mirrored numbers to
//! the tokens they came from.

use std::sync::LazyLock;

use base64::Engine as _;
use dioxus::desktop::tao::window::{Icon, Window};
use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;

use crate::engine::{Command, Controller, Frame, Link, Ping, Reveal, Secret, State, bytes, rate};
use crate::profile_file::Subject;
use bliss_proto::{Action, ProxyMode};

#[cfg(target_os = "linux")]
#[path = "tray_linux.rs"]
mod tray_linux;
#[cfg(target_os = "linux")]
use tray_linux::Tray;

/// The mark alone, 96 px: the window's icon and the tray's off Windows, where
/// there are no resources to take it from (on Windows it is `bliss.ico`'s
/// frame of the same size).
#[cfg(not(windows))]
pub(crate) const MARK: &[u8] = include_bytes!("../assets/bliss-mark.png");

/// A second copy's asks to show the window (`instance.rs`), for `Tray` to take.
#[cfg(target_os = "linux")]
static RAISES: std::sync::Mutex<Option<Raises>> = std::sync::Mutex::new(None);

/// Whether something on this desktop draws tray icons, decided once at launch
/// (`tray_linux::host`). Without one a window hidden in the tray could not be
/// brought back by anything but starting bliss again, so it closes instead.
#[cfg(target_os = "linux")]
static TRAY_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window, built hidden on Linux as dioxus-desktop builds it on Windows,
/// is to be shown once its page exists (`App`): every start but one in the
/// tray.
#[cfg(target_os = "linux")]
static SHOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A start in the tray on a desktop with no tray: the window opens minimized
/// instead, once its page exists (`App`), never before — rule 20 holds for
/// every webview here, whatever WebKitGTK would put up with.
#[cfg(target_os = "linux")]
static ICONIFY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window's palette, type and radii — a calm neon; see theme.css.
const THEME: &str = include_str!("../assets/theme.css");
const APP_CSS: &str = include_str!("../assets/app.css");
/// The status card's night scene; see `.status-card` in app.css.
const SCENE: &[u8] = include_bytes!("../assets/status-card.jpg");
/// How app.css names the scene, for `STYLES` to replace.
const SCENE_URL: &str = "url(status-card.jpg)";
/// The cat asleep at the foot of the rail; see `.rail-cat` in app.css.
const RAIL_CAT: &[u8] = include_bytes!("../assets/rail-cat.jpg");
/// How app.css names the cat, for `STYLES` to replace.
const RAIL_CAT_URL: &str = "url(rail-cat.jpg)";
/// The bunny in the corner of the tray's menu; see `.tray-card` in app.css.
const TRAY_BUNNY: &[u8] = include_bytes!("../assets/tray-bunny.jpg");
/// How app.css names the bunny, for `STYLES` to replace.
const TRAY_BUNNY_URL: &str = "url(tray-bunny.jpg)";
/// Comfortaa, subset to what the window draws; see the @font-face in theme.css.
const FONT: &[u8] = include_bytes!("../assets/comfortaa.woff2");
/// How theme.css names the font, for `THEME_STYLES` to replace.
const FONT_URL: &str = "url(comfortaa.woff2)";

/// A `url(...)` holding the file's bytes, so the file ships inside bliss.exe
/// the way the stylesheets do instead of having to sit next to it.
fn data_url(mime: &str, bytes: &[u8]) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("url(data:{mime};base64,{data})")
}

/// app.css with its pictures inlined. Built once: encoding a picture on every
/// render would be work for nothing.
static STYLES: LazyLock<String> = LazyLock::new(|| {
    APP_CSS
        .replace(SCENE_URL, &data_url("image/jpeg", SCENE))
        .replace(RAIL_CAT_URL, &data_url("image/jpeg", RAIL_CAT))
        .replace(TRAY_BUNNY_URL, &data_url("image/jpeg", TRAY_BUNNY))
});

/// theme.css with the font inlined. Nothing is fetched: the window opens before
/// the tunnel, for people whose network may well block a font CDN.
static THEME_STYLES: LazyLock<String> =
    LazyLock::new(|| THEME.replace(FONT_URL, &data_url("font/woff2", FONT)));

/// This file, so a test can check the styles in it rather than trust a rule to
/// be remembered. See `radius_literals_match_the_design_tokens`.
#[cfg(test)]
const SOURCE: &str = include_str!("ui.rs");

/// The wrapper every screen sits in, so the fade-in has something to animate.
/// `min-width:0` so a line that must not wrap clips inside the screen instead
/// of stretching it past the window's edge.
const SCREEN: &str = "flex:1;min-height:0;min-width:0;display:flex;flex-direction:column";

/// How wide a digit's box is, in ems, for a number that changes while someone
/// is looking at it. `.figure` in app.css is the box; this is its width.
///
/// Comfortaa has no tabular figures. Every `font-variant-numeric:tabular-nums`
/// in this file has therefore never done anything: the property asks for a
/// `tnum` feature the file does not carry, and the font's digits run from 378
/// to 647 units of 1000 — a spread of 0.269em. At 14px the clock on the status
/// card moved the rest of its line by up to twenty pixels a second, which the
/// user reported as text hopping about. A digit centred in a box of the widest
/// digit's width *is* a tabular figure, so that is what `Ticking` draws.
///
/// The width itself lives in `.figure` in app.css, where a box belongs; this
/// is the measurement it has to keep up with, and a test holds the two
/// together.
#[cfg(test)]
const FIGURE_EM: &str = "0.65";
/// Soft pink, `--color-accent` in theme.css: what is on, chosen or clickable.
const ACCENT: &str = "#ee82c4";
/// Neutral-400: window controls and anything else that should recede.
const MUTED: &str = "#b5a9c4";
/// `--color-good`: a quick server, a rule that sends traffic direct.
const GOOD: &str = "#72cf9e";
/// `--color-bad`: a failure, a server that did not answer, a blocking rule.
const BAD: &str = "#e56b6f";
/// `--color-vpn`: the work VPN, and a rule that sends traffic through it.
const VPN_TONE: &str = "#7eb3ee";

// Status is carried by colour rather than by a word, so these are the only
// place the state is spelled out: border, outer glow, inner bloom. Connected is
// the accent with a soft glow, as in the mockup the theme came from; the glow
// is the whole "calm neon", and it stays faint on purpose.
const ON: (&str, &str, &str) = (ACCENT, "rgba(238,130,196,.34)", "rgba(238,130,196,.14)");
// Starting gets violet rather than a premature pink: it is not connected yet.
const BUSY: (&str, &str, &str) = ("#a98ce8", "rgba(169,140,232,.26)", "rgba(169,140,232,.12)");
// Off is quiet rather than alarming: a tunnel someone turned off is not a problem.
const IDLE: (&str, &str, &str) = ("#766a88", "rgba(118,106,136,.16)", "rgba(118,106,136,.08)");
// A failure is the one state that asks to be looked at.
const FAILED: (&str, &str, &str) = (BAD, "rgba(229,107,111,.26)", "rgba(229,107,111,.12)");

// The spacing scale, which every screen keeps: 12px between any two cards —
// the rail and the screen beside it too, so a screen's left padding is 12px
// and its right 20px, the rail's own distance from the window's edge — 18px
// above and below, 16px inside a card, 12px inside a tile within a card. The
// cards had grown 14, 15, 18 and 20px of their own, side by side on one
// screen, and the rail stood 20px from everything; the user saw both.

/// A screen's heading row: as tall as a heading with a button beside it, so
/// the first card starts at one height on every screen.
const SCREEN_HEAD: &str = "display:flex;align-items:baseline;gap:10px;min-height:34px;flex:none";
/// A screen's scrolling column. It reaches into the screen's right padding by
/// its scrollbar's width (9px, and 3px to spare), so its cards end where
/// every other screen's do rather than 9px short of them.
const SCROLL_COLUMN: &str =
    "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;margin-right:-12px;padding-right:3px";

/// The log pane's height on the logs screen, in CSS pixels. Global, because
/// the screen is built afresh on every visit and the height should outlive it.
static LOG_HEIGHT: GlobalSignal<f64> = Signal::global(|| DEFAULT_LOG_HEIGHT);
const DEFAULT_LOG_HEIGHT: f64 = 230.0;
const MIN_LOG_HEIGHT: f64 = 110.0;
/// The server list's own width, as its header measures it, and the two
/// columns someone dragged to a width of their own (`None`: the default
/// share). Global for the reason `LOG_HEIGHT` is.
static SERVER_LIST_WIDTH: GlobalSignal<f64> = Signal::global(|| 0.0);
static SERVER_NAME_WIDTH: GlobalSignal<Option<f64>> = Signal::global(|| None);
static SERVER_KIND_WIDTH: GlobalSignal<Option<f64>> = Signal::global(|| None);
/// What the connections table is narrowed to, and in which order.
static TABLE_QUERY: GlobalSignal<String> = Signal::global(String::new);
static TABLE_SORT: GlobalSignal<Sort> = Signal::global(|| Sort::Newest);
/// The right-click menu over a connection, while it is open. Global so Esc,
/// heard at the top of the window, can close it.
static ROW_MENU: GlobalSignal<Option<RowMenu>> = Signal::global(|| None);
/// The rule editor, while it is open. Global for the same reason.
static DRAFT: GlobalSignal<Option<Draft>> = Signal::global(|| None);
/// The subscription editor, while it is open. Global for the same reason.
static SUB_DRAFT: GlobalSignal<Option<SubDraft>> = Signal::global(|| None);
/// The rule the connections table is narrowed to; `None` for every rule.
static TABLE_RULE: GlobalSignal<Option<String>> = Signal::global(|| None);

/// Whether the window is lent to the tray's menu rather than showing the
/// program, and the shape to give it back in (`into_menu`, `out_of_menu`).
static TRAY_MENU: GlobalSignal<bool> = Signal::global(|| false);
/// Whether the menu has had the focus yet. It closes when the focus goes, but
/// the first thing that arrives after it opens is the losing of the focus it
/// never had: shown from a click on the tray, the window is not the foreground
/// one, and it closed itself before it had been seen.
static MENU_HELD: GlobalSignal<bool> = Signal::global(|| false);
/// When it opened. Windows hands the focus around as a window appears, and a
/// menu that took the first loss of it for an answer closed before it was seen.
static MENU_OPENED: GlobalSignal<Option<std::time::Instant>> = Signal::global(|| None);
static NORMAL_SHAPE: GlobalSignal<Option<(i32, i32, u32, u32)>> = Signal::global(|| None);

/// Whether the page has let frames go by because nobody could see it, and so
/// has to be brought up to date before anyone can (`catch_up`). Not a signal:
/// nothing draws from it.
static BEHIND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Asks for the program's window to come back out of the tray, counting up
/// per ask; `Tray` shows it once the page has painted the program.
static BRING_BACK: GlobalSignal<u64> = Signal::global(|| 0);
/// Whether the connections screen is open. The table goes into the frame
/// while it is and the page can be seen; in the tray it is let go as well,
/// and asked for again on the way back (`catch_up`).
static TABLE_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The tray icon's tooltip. The icon is seen with the window in the tray,
/// which is when the page gets no frames; this is the one thing that still
/// follows them there, and a change of it re-runs `Tray` and nothing else.
static TOOLTIP: GlobalSignal<String> = Signal::global(String::new);
/// Whether the list of rules to narrow the table to is open.
static RULE_MENU: GlobalSignal<bool> = Signal::global(|| false);
/// Subscription groups in the server list someone opened or closed, by id.
/// The rest are open only while they hold the server in use.
static GROUP_OPEN: GlobalSignal<std::collections::HashMap<String, bool>> =
    Signal::global(std::collections::HashMap::new);

/// How the connections table is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sort {
    /// As the engine hands the rows over: live first, newest first.
    Newest,
    DownMost,
    DownLeast,
    UpMost,
    UpLeast,
}

/// A connection someone right-clicked, and where.
#[derive(Clone, Debug, PartialEq)]
struct RowMenu {
    x: f64,
    y: f64,
    target: String,
    host: String,
    process: Option<String>,
    route: bliss_core::Route,
    rule: String,
    /// The row as the clipboard gets it.
    text: String,
}

/// A rule in the editor, before it is saved.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    /// The rule it changes, by position from 0; `None` for a new rule.
    at: Option<usize>,
    name: String,
    action: Action,
    /// One condition to a line, as `rule_text` reads them.
    conditions: String,
    /// Why the last attempt to save was refused.
    problem: Option<String>,
    /// "Удалить" has been clicked once; the second click deletes.
    deleting: bool,
}

/// A subscription in the editor, before it is saved.
///
/// No `Debug`: `url` is an address being typed, with its token in it.
#[derive(Clone, PartialEq)]
struct SubDraft {
    id: String,
    name: String,
    /// A new address; left empty, the old one stays. The old one is never put
    /// here, or anywhere in the window: only its host is shown.
    url: String,
    /// The old one as the card shows it, for the box's placeholder.
    shown_url: String,
    /// Why the last attempt to save was refused.
    problem: Option<String>,
}

/// Keys that mean something anywhere in the window: Ctrl+V or Shift+Insert
/// adds whatever is on the clipboard, and Esc closes whatever is open.
///
/// Heard in the page itself. A key goes to whatever has focus, and with nothing
/// focused that is the document — outside every element rsx can put a handler
/// on, which is where the window is most of the time. Blitz runs no script, so
/// a Blitz build would need these keys some other way.
const KEY_LISTENER: &str = r#"
window.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
        dioxus.send("escape");
        return;
    }
    // By position, not by letter: on a Russian layout the V key types "м".
    const paste = (event.ctrlKey && !event.altKey && !event.shiftKey && event.code === "KeyV")
        || (event.shiftKey && !event.ctrlKey && !event.altKey && event.code === "Insert");
    if (!paste || event.repeat) return;
    const target = event.target;
    const typing = target
        && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    // A text box pastes into itself, and an open dialog is somebody's work.
    if (typing || document.querySelector("[data-modal]")) return;
    event.preventDefault();
    dioxus.send("paste");
});
await new Promise(() => {});
"#;

/// What the traffic chart says under the pointer: a guide, a dot on the sample
/// nearest it, and that sample's rate, from the chart's `data-rates` and
/// `data-labels`.
///
/// A script, placing them on every frame while the pointer is over the chart:
/// the samples glide out from under a pointer that holds still, which no
/// handler in rsx would hear, and only the page knows where the glide stands.
/// It writes custom properties and `data-hover` on `.chart`, to which rsx gives
/// no style and no such attribute, so no render takes them back. Only the
/// samples on the chart are read, not the one past each edge. Blitz runs no
/// script, and would show no readout.
const CHART_HOVER: &str = r#"
if (!window.blissChartHover) {
    window.blissChartHover = true;
    let pointer = null;
    let frame = 0;
    const matrix = (element) => {
        const transform = getComputedStyle(element).transform;
        return new DOMMatrixReadOnly(transform === "none" ? undefined : transform);
    };
    const hide = () => {
        pointer = null;
        for (const chart of document.querySelectorAll(".chart[data-hover]")) {
            chart.removeAttribute("data-hover");
        }
    };
    const draw = () => {
        frame = 0;
        if (!pointer) return;
        const under = document.elementFromPoint(pointer.x, pointer.y);
        const chart = under && under.closest(".chart");
        const layer = chart && chart.querySelector(".glide");
        const group = chart && chart.querySelector(".chart-scale");
        const tip = chart && chart.querySelector(".chart-tip");
        const rates = chart ? (chart.dataset.rates || "").split(",").map(Number) : [];
        const labels = chart ? (chart.dataset.labels || "").split("|") : [];
        if (!layer || !group || !tip || rates.length < 3) {
            hide();
            return;
        }
        const box = chart.getBoundingClientRect();
        const shift = matrix(layer).m41;
        const step = layer.offsetWidth / (rates.length - 1);
        const first = Math.ceil(-shift / step - 0.001);
        const last = Math.floor((box.width - shift) / step + 0.001);
        const k = Math.max(first, Math.min(last, Math.round((pointer.x - box.left - shift) / step)));
        const x = shift + k * step;
        // The group maps a KB/s to its height out of the chart's 100 units.
        const y = (1 + matrix(group).m22 * rates[k] / 1024 / 100) * layer.offsetHeight;
        tip.textContent = labels[k] || "";
        const tipX = x + 12 + tip.offsetWidth <= box.width ? x + 12 : x - 12 - tip.offsetWidth;
        const tipY = Math.max(0, Math.min(box.height - tip.offsetHeight, y - tip.offsetHeight - 10));
        chart.style.setProperty("--hover-x", x + "px");
        chart.style.setProperty("--hover-y", y + "px");
        chart.style.setProperty("--tip-x", tipX + "px");
        chart.style.setProperty("--tip-y", tipY + "px");
        chart.setAttribute("data-hover", "");
        frame = requestAnimationFrame(draw);
    };
    document.addEventListener("pointermove", (event) => {
        pointer = { x: event.clientX, y: event.clientY };
        if (!frame) frame = requestAnimationFrame(draw);
    }, { passive: true });
    document.documentElement.addEventListener("pointerleave", hide);
    window.addEventListener("blur", hide);
}
"#;

/// A second copy's asks to show the window: the activation token each came
/// with, if any.
pub type Raises = tokio::sync::mpsc::UnboundedReceiver<Option<String>>;

/// Open the window, or leave it in the tray: `--minimized` is what the session
/// passes when it starts bliss at sign-in and the settings ask for that.
///
/// Never returns: the event loop ends the process itself, after `LoopDestroyed`
/// — which is where the system proxy is put back on the way out. Code after
/// this call, which is where it used to be, never ran: «Выход» left the machine
/// pointed at a port nobody listened on until bliss next started.
pub fn launch(
    controller: Controller,
    minimized: bool,
    work_dir: std::path::PathBuf,
    raises: Option<Raises>,
) {
    #[cfg(target_os = "linux")]
    let tray = {
        if let Ok(mut held) = RAISES.lock() {
            *held = raises;
        }
        // A start in the tray is the one at sign-in, which can come before
        // the desktop's tray does: that one waits for it a while.
        let host = tray_linux::host(minimized);
        SHOW.store(!minimized || !host, std::sync::atomic::Ordering::Relaxed);
        ICONIFY.store(minimized && !host, std::sync::atomic::Ordering::Relaxed);
        if !host {
            tracing::info!(
                "no tray on this desktop: closing the window leaves bliss, and a start \
                 in the tray opens minimized instead"
            );
        }
        TRAY_HOST.store(host, std::sync::atomic::Ordering::Relaxed);
        host
    };
    #[cfg(not(target_os = "linux"))]
    let tray = {
        let _ = raises;
        true
    };

    let config = Config::new()
        .with_window(
            WindowBuilder::new()
                .with_title("bliss")
                // The design draws its own chrome, so Windows must not draw
                // any. Everything the frame used to do — drag, minimise,
                // maximise, close — is re-implemented in TitleBar below.
                .with_decorations(false)
                .with_inner_size(LogicalSize::new(1040.0, 660.0))
                .with_min_inner_size(LogicalSize::new(880.0, 560.0))
                // Started in the tray, the window is simply never
                // shown. dioxus-desktop builds every window hidden and
                // shows it after the first render, so the webview is
                // created in the window Windows is happy with either
                // way; minimized instead, WebView2 refuses to be
                // created at all (rule 20 of AGENTS.md §6).
                //
                // On Linux dioxus builds it as asked, and bliss always asks
                // for hidden and shows it itself (`SHOW`): a page in a window
                // built visible applies each change on an animation frame,
                // which a hidden window never gets, and the whole program
                // stood still once it went into the tray (rule 27).
                .with_visible(!minimized && cfg!(not(target_os = "linux")))
                // Dioxus puts its own logo on any window built without one.
                .with_window_icon(app_icon(None)),
        )
        // The window is the program's, not a document's: its close
        // button and Alt+F4 put it in the tray, where bliss goes on
        // serving. Leaving is a deliberate act, from the tray (`Tray`).
        // A desktop with no tray has nowhere to put it, and there
        // closing the window is leaving.
        .with_close_behaviour(if tray {
            dioxus::desktop::WindowCloseBehaviour::WindowHides
        } else {
            dioxus::desktop::WindowCloseBehaviour::WindowCloses
        })
        // The clicks on the icon are answered here, not by Dioxus.
        .with_tray_icon_show_window_on_click(false)
        .with_on_window(|window, _| set_icons(&window, window.scale_factor()))
        // The last thing the event loop does before it ends the process.
        .with_custom_event_handler(move |event, _| {
            if let dioxus::desktop::tao::event::Event::LoopDestroyed = event
                && let Err(e) = bliss_core::sysproxy::leave(&work_dir)
            {
                tracing::error!("could not restore the system proxy on the way out: {e}");
            }
        });
    #[cfg(target_os = "linux")]
    let config = linux_config(config);
    dioxus::LaunchBuilder::desktop()
        .with_cfg(config)
        .with_context(controller)
        .launch(App);
}

/// What Linux's webview needs said that Windows' does not.
///
/// - Dioxus's switch for WebKitGTK's GPU path changes the environment after
///   threads exist and sends every child to X11; `main` has made the same
///   choice already, first thing (`webkit_environment`).
/// - Without a folder of its own, WebKit keeps its data in
///   `~/.local/share/<program>` — which for bliss is the work dir. It goes
///   under the cache instead, named after the executable as WebView2's is.
/// - The window is shown before the page has loaded, unlike on Windows, so
///   its ground is the page's colour rather than a white flash.
#[cfg(target_os = "linux")]
fn linux_config(config: Config) -> Config {
    let stem = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "bliss".to_owned());
    let mut config = config
        .with_disable_dma_buf_on_wayland(false)
        .with_background_color((0x14, 0x0f, 0x1d, 0xff));
    if let Some(base) = directories::BaseDirs::new() {
        config = config.with_data_directory(base.cache_dir().join(stem).join("webview"));
    }
    config
}

/// The icon build.rs compiles into bliss.exe (resource 1 in bliss.rc), with a
/// side in physical pixels, or at the system's default size.
#[cfg(windows)]
fn app_icon(side: Option<u32>) -> Option<Icon> {
    use dioxus::desktop::tao::dpi::PhysicalSize;
    use dioxus::desktop::tao::platform::windows::IconExtWindows;

    Icon::from_resource(1, side.map(|side| PhysicalSize::new(side, side)))
        .inspect_err(|e| tracing::warn!("could not load the application icon: {e}"))
        .ok()
}

/// The mark from the PNG built into the binary, whatever side is asked: X11
/// scales it, and a Wayland desktop takes the icon from the program's
/// `.desktop` entry rather than from the window.
#[cfg(not(windows))]
fn app_icon(_side: Option<u32>) -> Option<Icon> {
    dioxus::desktop::icon_from_memory(MARK)
        .inspect_err(|e| tracing::warn!("could not load the application icon: {e}"))
        .ok()
}

/// bliss's icon beside the clock, and what clicking it does.
///
/// Built here rather than through Dioxus's `init_tray_icon` because there is no
/// native menu to give it: Windows draws those in its own font and colours, and
/// bliss's own menu is this window in a compact shape (`into_menu`).
///
/// Its tooltip comes from `TOOLTIP`, not from the frame, so this runs only when
/// the words change, which is exactly when the shell has to be told (rule 17
/// of AGENTS.md §6) — and goes on running in the tray, where the page gets no
/// frames. The frame's signal is here to be brought up to date before the
/// window is shown (`catch_up`).
#[cfg(windows)]
#[component]
fn Tray(frame: Signal<Frame>) -> Element {
    use dioxus::desktop::trayicon::{
        MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    };

    let controller = use_context::<Controller>();
    let window = dioxus::desktop::use_window();
    // The engine's words, or, before its first frame, the ones in the frame
    // the page started with.
    let tooltip = match TOOLTIP() {
        words if words.is_empty() => tray_tooltip(&frame.peek()),
        words => words,
    };
    let side = icon_side(16.0, window.scale_factor());
    let icon = use_hook(|| {
        // No menu of Windows' own, and not only because it cannot be styled:
        // `TrackPopupMenu` runs a loop of its own inside the shell's message,
        // which holds up every event this window has to answer — including the
        // one that puts bliss's own menu on the screen. With a menu attached,
        // a right click opened that and nothing else ever happened.
        let mut builder = TrayIconBuilder::new().with_tooltip("bliss");
        if let Some(mark) = tray_mark(side) {
            builder = builder.with_icon(mark);
        }
        builder
            .build()
            .inspect_err(|e| tracing::warn!("no tray icon: {e}"))
            .ok()
            .map(std::rc::Rc::new)
    });
    if let Some(icon) = &icon {
        let _ = icon.set_tooltip(Some(&tooltip));
    }

    // The menu is shown only once the page has painted it: shown cloaked, so
    // WebView2 paints into a window that is up, and uncloaked after the paint
    // (`cloaked`, `when_painted`; see `into_menu`).
    let showing = window.clone();
    use_effect(move || {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        if TRAY_MENU() {
            cloaked(&showing, true);
            showing.set_visible(true);
            let _ = showing.set_skip_taskbar(true);
            let showing = showing.clone();
            when_painted(move || {
                cloaked(&showing, false);
                // Esc, or another click, may have put it away meanwhile.
                if *TRAY_MENU.peek() {
                    showing.set_focus();
                }
            });
        }
    });

    // And the program, the same way, when it comes back out of the tray
    // (`out_of_the_tray`). The count says a new ask has come.
    let bringing = window.clone();
    let answered = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(0u64)));
    use_effect(move || {
        let asked = BRING_BACK();
        if asked == answered.get() {
            return;
        }
        answered.set(asked);
        // Gone into the menu meanwhile: the menu shows itself.
        if *TRAY_MENU.peek() {
            return;
        }
        cloaked(&bringing, true);
        bringing.set_visible(true);
        bringing.set_minimized(false);
        let window = bringing.clone();
        when_painted(move || {
            cloaked(&window, false);
            window.set_focus();
        });
    });

    let clicked = window.clone();
    let asking = controller.clone();
    dioxus::desktop::use_tray_icon_event_handler(move |event| {
        // On the release: a click is a press and a release, and both arrive.
        let TrayIconEvent::Click {
            button,
            button_state: MouseButtonState::Up,
            position,
            ..
        } = event
        else {
            return;
        };
        match button {
            // The left button is for the program itself.
            MouseButton::Left => {
                if TRAY_MENU() {
                    out_of_menu(&clicked, true);
                } else {
                    catch_up(&clicked, frame, &asking);
                    out_of_the_tray(&clicked);
                }
            }
            // The right one is for the menu — unless the program is already
            // open, in which case raising it is the useful answer and taking
            // its window away to make a menu of it would not be.
            MouseButton::Right => {
                if TRAY_MENU() {
                    out_of_menu(&clicked, false);
                } else if clicked.is_visible() {
                    catch_up(&clicked, frame, &asking);
                    out_of_the_tray(&clicked);
                } else {
                    // Up to date before the menu is drawn from it, in the
                    // same render that puts the menu on the page.
                    catch_up(&clicked, frame, &asking);
                    // Read now, not captured: this handler is made once,
                    // and the presets change under it.
                    let presets = frame.peek().routing.presets.len();
                    into_menu(&clicked, (position.x, position.y), presets);
                }
            }
            MouseButton::Middle => {}
        }
    });

    // Back from the taskbar, or anywhere else bliss did not bring it back
    // from itself: up to date at once rather than at the next frame.
    let restored = window.clone();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        if let Event::WindowEvent {
            event: WindowEvent::Focused(true) | WindowEvent::Resized(_),
            ..
        } = event
            && seen(&restored)
        {
            catch_up(&restored, frame, &controller);
        }
    });

    // A menu closes when you look elsewhere — once it has been looked at.
    let unfocused = window.clone();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        let Event::WindowEvent {
            event: WindowEvent::Focused(focused),
            ..
        } = event
        else {
            return;
        };
        if !TRAY_MENU() {
            return;
        }
        let settled =
            MENU_OPENED().is_some_and(|at| at.elapsed() > std::time::Duration::from_millis(250));
        if *focused {
            *MENU_HELD.write() = true;
        } else if MENU_HELD() && settled {
            out_of_menu(&unfocused, false);
        }
    });

    rsx! {}
}

/// Neither Windows nor Linux: no tray.
#[cfg(not(any(windows, target_os = "linux")))]
#[component]
fn Tray(frame: Signal<Frame>) -> Element {
    let _ = frame;
    rsx! {}
}

/// How tall the menu is with this many presets: its own shape, and a heading
/// and a line for each preset, four at most — more scroll (`.tray-presets`).
/// Grown rather than taken from the server list, which is what the menu was
/// made taller for.
#[cfg_attr(not(windows), allow(dead_code))]
fn menu_height(presets: usize) -> f64 {
    match presets {
        0 => MENU_SIZE.1,
        n => MENU_SIZE.1 + 37.0 + PRESET_LINE * n.min(PRESET_LINES) as f64,
    }
}

/// A preset's line in the menu, gap included, and how many show before the
/// list scrolls; `.tray-presets` in app.css is that tall, less the last gap.
#[cfg_attr(not(windows), allow(dead_code))]
const PRESET_LINE: f64 = 28.0;
#[cfg_attr(not(windows), allow(dead_code))]
const PRESET_LINES: usize = 4;

/// How big the tray's menu is, in logical pixels, and the smallest the program
/// itself may be — the menu has to undo that floor to fit.
///
/// Everything but the server list takes about 270px (Comfortaa's line is
/// 1.115em); the rest is the list's, up to `.tray-list`'s `max-height` in
/// app.css. At 372px that was three servers and a half, and the user asked for
/// more: 460px holds six and the edge of a seventh, which says it scrolls.
#[cfg_attr(not(windows), allow(dead_code))]
const MENU_SIZE: (f64, f64) = (300.0, 460.0);
const MIN_SIZE: (f64, f64) = (880.0, 560.0);

/// Lend the window to the tray's menu: small, above everything, out of the
/// taskbar, beside the icon, with the program's screen folded away under it.
#[cfg_attr(not(windows), allow(dead_code))]
fn into_menu(window: &dioxus::desktop::DesktopContext, near: (f64, f64), presets: usize) {
    use dioxus::desktop::tao::dpi::LogicalSize;

    if !TRAY_MENU() {
        let size = window.inner_size();
        if let Ok(at) = window.outer_position() {
            *NORMAL_SHAPE.write() = Some((at.x, at.y, size.width, size.height));
        }
    }
    *TRAY_MENU.write() = true;
    *MENU_HELD.write() = false;
    *MENU_OPENED.write() = Some(std::time::Instant::now());
    // Shaped out of sight, and shown by the effect in `Tray` once the page has
    // drawn the menu. Shown first, the window stood there for a frame holding
    // the whole program shrunk into 300 pixels.
    window.set_visible(false);
    window.set_always_on_top(true);
    // A menu has one shape. The window has no frame, but tao still lets a
    // borderless window be dragged by its edges while it is resizable (its
    // WM_NCHITTEST), and Win+Up maximizes anything with a maximize box — the
    // menu stretched either way. Both come back in `out_of_menu`.
    window.set_resizable(false);
    window.set_maximizable(false);
    window.set_min_inner_size(None::<LogicalSize<f64>>);
    let height = menu_height(presets);
    window.set_inner_size(LogicalSize::new(MENU_SIZE.0, height));
    window.set_outer_position(menu_corner(window, near, height));
}

/// Give the window back: the program's shape, and shown or away.
fn out_of_menu(window: &dioxus::desktop::DesktopContext, show: bool) {
    use dioxus::desktop::tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};

    *TRAY_MENU.write() = false;
    *MENU_HELD.write() = false;
    *MENU_OPENED.write() = None;
    // Out of sight before it changes shape. Given its size and place back while
    // still on screen, the window flashed the whole program in the middle of
    // the screen for a frame — on the way out of the menu and out of bliss.
    // And out of sight at once rather than faded, or the fade follows the
    // window to its new place (see `animated`).
    animated(window, false);
    window.set_visible(false);
    window.set_always_on_top(false);
    #[cfg(windows)]
    {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        let _ = window.set_skip_taskbar(false);
    }
    window.set_resizable(true);
    window.set_maximizable(true);
    window.set_min_inner_size(Some(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1)));
    if let Some((x, y, width, height)) = NORMAL_SHAPE() {
        window.set_inner_size(PhysicalSize::new(width, height));
        window.set_outer_position(PhysicalPosition::new(x, y));
    }
    // Back on for the program, which opens and minimizes the way windows do.
    animated(window, true);
    if show {
        out_of_the_tray(window);
    }
}

/// Let Windows animate the window's showing and hiding, or not.
///
/// Under the frame bliss draws, the window keeps a caption style (tao keeps
/// it, for the shadow and for snapping), so Windows fades it out when it is
/// hidden — and the fade follows the window. `out_of_menu` hides the menu and
/// gives the window back the program's place in the same breath, so the
/// fading menu flashed wherever the program stands, the top-left corner for a
/// bliss started in the tray; the user saw it on every click away from the
/// menu. Hidden with the fade off, the menu goes at once, the way a menu does.
#[cfg(windows)]
fn animated(window: &dioxus::desktop::DesktopContext, on: bool) {
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_TRANSITIONS_FORCEDISABLED, DwmSetWindowAttribute};

    // A Win32 BOOL, four bytes: true turns the animations off.
    let off = i32::from(!on);
    // SAFETY: the handle is this window's own and lives as long as its
    // context; the value is the four bytes the attribute is documented to take.
    let set = unsafe {
        DwmSetWindowAttribute(
            HWND(window.hwnd() as *mut core::ffi::c_void),
            DWMWA_TRANSITIONS_FORCEDISABLED,
            (&raw const off).cast(),
            size_of::<i32>() as u32,
        )
    };
    if let Err(e) = set {
        tracing::debug!("window animations not switched: {e}");
    }
}

#[cfg(not(windows))]
fn animated(_window: &dioxus::desktop::DesktopContext, _on: bool) {}

/// Keep a shown window off the screen, or let the screen show it.
///
/// A cloaked window is up — shown, sized, given its paints — and DWM simply
/// leaves it out of the picture. What the screen shows when the cloak comes
/// off is then the page as WebView2 painted it while cloaked, never what it
/// presented before it was hidden (`when_painted`).
#[cfg(windows)]
fn cloaked(window: &dioxus::desktop::DesktopContext, on: bool) {
    use dioxus::desktop::tao::platform::windows::WindowExtWindows;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};

    // A Win32 BOOL, four bytes.
    let cloak = i32::from(on);
    // SAFETY: the handle is this window's own and lives as long as its
    // context; the value is the four bytes the attribute is documented to take.
    let set = unsafe {
        DwmSetWindowAttribute(
            HWND(window.hwnd() as *mut core::ffi::c_void),
            DWMWA_CLOAK,
            (&raw const cloak).cast(),
            size_of::<i32>() as u32,
        )
    };
    if let Err(e) = set {
        tracing::debug!("window not {}cloaked: {e}", if on { "" } else { "un" });
    }
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn cloaked(_window: &dioxus::desktop::DesktopContext, _on: bool) {}

/// Put the menu by the icon and keep it on the screen the pointer is on: a
/// menu half off the edge is worse than one a little away from the click.
#[cfg_attr(not(windows), allow(dead_code))]
fn menu_corner(
    window: &dioxus::desktop::DesktopContext,
    near: (f64, f64),
    height: f64,
) -> dioxus::desktop::tao::dpi::PhysicalPosition<i32> {
    use dioxus::desktop::tao::dpi::PhysicalPosition;

    let scale = window.scale_factor();
    let (width, height) = ((MENU_SIZE.0 * scale) as i32, (height * scale) as i32);
    let (x, y) = (near.0 as i32, near.1 as i32);
    let screen = window
        .available_monitors()
        .find(|monitor| {
            let at = monitor.position();
            let size = monitor.size();
            (at.x..at.x + size.width as i32).contains(&x)
                && (at.y..at.y + size.height as i32).contains(&y)
        })
        .or_else(|| window.current_monitor());
    let Some(screen) = screen else {
        return PhysicalPosition::new(x - width / 2, y - height - 12);
    };
    let at = screen.position();
    let size = screen.size();
    let margin = (12.0 * scale) as i32;
    let left = (x - width / 2).clamp(at.x + margin, at.x + size.width as i32 - width - margin);
    // Above the pointer, which is over the tray; below it if there is no room.
    let above = y - height - margin;
    let top = if above >= at.y + margin {
        above
    } else {
        (y + margin).min(at.y + size.height as i32 - height - margin)
    };
    PhysicalPosition::new(left, top)
}

/// Put the menu away, whatever was chosen in it.
fn close_menu() {
    out_of_menu(&dioxus::desktop::window(), false);
}

/// End the program. The window hides when it closes, so leaving has to say
/// otherwise first; then it is the last window, and that ends the program.
fn quit(window: &dioxus::desktop::DesktopContext) {
    // Away first: closing while the menu was up put the window back into its
    // full shape on the way out, which is a flash of the program at the centre
    // of the screen as bliss leaves.
    *TRAY_MENU.write() = false;
    window.set_visible(false);
    window.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
    window.close();
}

/// Leave as «Выход» does when asked to by a signal: SIGTERM, which ends
/// every program of a session as it closes, SIGHUP, and SIGINT from a
/// terminal. Unheard, a signal ended the process where it stood, nothing put
/// the proxy back, and the next session started on bliss's dead port. glib
/// hears them on this thread, between two events, and the window closes as
/// the last one does: `LoopDestroyed` puts the settings back.
#[cfg(target_os = "linux")]
fn leave_on_signals(window: &dioxus::desktop::DesktopContext) {
    for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
        let window = window.clone();
        gtk::glib::unix_signal_add_local(signal, move || {
            tracing::info!("signal {signal}: leaving");
            window.set_close_behavior(dioxus::desktop::WindowCloseBehaviour::WindowCloses);
            window.close();
            gtk::glib::ControlFlow::Continue
        });
    }
}

/// The mark out of bliss.exe's own resources, at the size the tray draws.
#[cfg(windows)]
fn tray_mark(side: u32) -> Option<dioxus::desktop::trayicon::Icon> {
    dioxus::desktop::trayicon::Icon::from_resource(1, Some((side, side)))
        .inspect_err(|e| tracing::warn!("could not load the tray icon: {e}"))
        .ok()
}

/// Whether anyone can see the page: the program's window on the screen, or
/// the tray's menu. A window behind others still counts; there is no cheap
/// way to know it is covered.
fn seen(window: &dioxus::desktop::DesktopContext) -> bool {
    *TRAY_MENU.peek() || (window.is_visible() && !window.is_minimized())
}

/// Bring a page that let frames go by up to date, before whatever is about to
/// show it does, and let WebView2 have its memory back.
fn catch_up(
    window: &dioxus::desktop::DesktopContext,
    mut frame: Signal<Frame>,
    controller: &Controller,
) {
    if BEHIND.swap(false, std::sync::atomic::Ordering::Relaxed) {
        wake(window, controller);
        frame.set(controller.frame());
    }
}

/// Undo what going out of sight did: WebView2's memory, and the table if its
/// screen is open. The table arrives a moment later, through the engine.
fn wake(window: &dioxus::desktop::DesktopContext, controller: &Controller) {
    rest(window, false);
    if TABLE_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
        controller.send(Command::Table(true));
    }
}

/// Let WebView2 give memory back while nobody can see the page, and ask for
/// it again when somebody can.
///
/// Microsoft's switch for an app that has gone inactive: its scripts go on
/// running — the tray's menu and the keys need them — and the browser
/// processes trim what they hold. Only a hint, and it lasts until it is
/// changed back, which `catch_up` and the frame loop do. It needs WebView2
/// 114 or later; an older one ignores it.
#[cfg(windows)]
fn rest(window: &dioxus::desktop::DesktopContext, idle: bool) {
    use dioxus::desktop::wry::{MemoryUsageLevel, WebViewExtWindows};
    let level = if idle {
        MemoryUsageLevel::Low
    } else {
        MemoryUsageLevel::Normal
    };
    if let Err(e) = window.webview.set_memory_usage_level(level) {
        tracing::debug!("webview memory level not changed: {e}");
    }
}

#[cfg(not(windows))]
fn rest(_window: &dioxus::desktop::DesktopContext, _idle: bool) {}

/// Bring the window back out of the tray.
///
/// From hidden, it is shown once the page has painted the program, by the
/// effect in `Tray` (`BRING_BACK`): shown at once, it stood for a frame with
/// whatever was painted last — the tray's menu, or the program in the menu's
/// shape. Already on the screen, or minimized, it shows the program as it is,
/// and is raised at once.
fn out_of_the_tray(window: &dioxus::desktop::DesktopContext) {
    // Linux shows and raises in one call, and has no cloak to paint under
    // (`tray_linux::present`).
    #[cfg(target_os = "linux")]
    {
        tray_linux::present(window, None);
        return;
    }
    #[allow(unreachable_code)]
    if window.is_visible() {
        window.set_minimized(false);
        window.set_focus();
    } else {
        *BRING_BACK.write() += 1;
    }
}

/// Answers once the page has painted what the last render put on it: the
/// first animation frame after a render comes before the paint that shows
/// it, the second after. Within 150ms whatever happens, so a page that is not
/// painting cannot keep the window away.
#[cfg_attr(not(windows), allow(dead_code))]
const PAINTED: &str = r#"
let answered = false;
const answer = () => {
    if (!answered) {
        answered = true;
        dioxus.send(true);
    }
};
let frames = 0;
const tick = () => {
    frames += 1;
    if (frames >= 4) {
        answer();
    } else {
        requestAnimationFrame(tick);
    }
};
requestAnimationFrame(tick);
setTimeout(answer, 200);
"#;

/// Do `then` once the page has painted (`PAINTED`).
///
/// Measured on 1 October by grabbing the screen under the tray's menu as it
/// appeared: for its first 10 to 70ms it was the program, cut to the menu's
/// shape, in every one of ten openings, in this build and in the one before
/// it. The render is Dioxus's, the paint is WebView2's and comes later, and
/// a window shown in between shows whatever was presented last. Waiting for
/// the paint alone was not enough: a hidden window's paints are not
/// presented, so the menu's first opening still showed the program. The
/// window is shown cloaked, painted into, and uncloaked here.
#[cfg_attr(not(windows), allow(dead_code))]
fn when_painted(then: impl FnOnce() + 'static) {
    let mut painted = document::eval(PAINTED);
    spawn(async move {
        let _ = painted.recv::<bool>().await;
        then();
    });
}

/// The tray's menu: the window itself, small, beside the icon.
///
/// Not a menu of Windows' own — those are drawn in the system's font and
/// colours, and this one is the program, in the program's look. Every item
/// gives the window back as it acts (`close_menu`), the way a menu closes.
#[component]
fn TrayMenu(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let on = frame.state.is_on();
    let (tone, outer, inner) = tones(frame.state);
    let chosen = frame.proxy_mode;
    let drifted = frame.routing.drifted;
    let presets: Vec<(String, bool)> = frame
        .routing
        .presets
        .iter()
        .map(|name| {
            let current = frame.routing.current_preset.as_deref() == Some(name.as_str());
            (name.clone(), current)
        })
        .collect();

    rsx! {
        div { class: "tray-card",
            div { style: "display:flex;align-items:center;gap:11px",
                div {
                    class: "ring",
                    style: "width:38px;height:38px;flex:none;border-radius:50%;display:grid;\
                            place-items:center;border:1px solid {tone};\
                            box-shadow:0 0 18px {outer}, inset 0 0 10px {inner}",
                    title: if on { "Отключить" } else { "Подключить" },
                    onclick: {
                        let controller = controller.clone();
                        move |_| controller.send(Command::Toggle { was_on: on })
                    },
                    crate::icons::Heart { size: 19, color: tone, filled: on }
                }
                div { style: "min-width:0;display:flex;flex-direction:column;gap:2px",
                    span {
                        style: "font:700 15px/1.1 var(--font-heading);color:{tone}",
                        "{frame.state.label()}"
                    }
                    span {
                        style: "font-size:12.5px;color:var(--color-neutral-500);\
                                min-width:0;overflow:hidden;white-space:nowrap",
                        "{elide(&frame.server_tag, 26)}"
                    }
                }
            }

            div { class: "tray-sep" }
            span { class: "tray-head", "Серверы" }
            div { class: "tray-list",
                for (index, server) in frame.servers.iter().enumerate() {
                    div {
                        key: "{index}",
                        // Marked by class, as everywhere else here: an inline
                        // property that comes and goes is restored and sticks.
                        class: if server.current { "srv current" } else { "srv" },
                        style: "display:flex;align-items:center;gap:8px;padding:6px 8px;\
                                border-radius:4px",
                        onclick: {
                            let controller = controller.clone();
                            move |_| {
                                controller.send(Command::Select(index));
                                close_menu();
                            }
                        },
                        span {
                            style: "flex:1;min-width:0;overflow:hidden;white-space:nowrap;\
                                    font-size:13.5px",
                            "{elide(&server.tag, 22)}"
                        }
                        span {
                            style: "font-size:12.5px;color:{ping_tone(server.ping)}",
                            "{ping_text(server.ping)}"
                        }
                    }
                }
            }

            if !presets.is_empty() {
                div { class: "tray-sep" }
                span { class: "tray-head", "Пресеты правил" }
                div { class: "tray-presets",
                    for (name, current) in presets {
                        div {
                            key: "{name}",
                            class: if current { "act choice on" } else { "act choice" },
                            style: "display:flex;align-items:center;gap:9px;padding:5px 8px;\
                                    border-radius:4px;font-size:13px;color:{choice_tint(current)}",
                            onclick: {
                                let controller = controller.clone();
                                let name = name.clone();
                                move |_| {
                                    controller.send(Command::ApplyPreset(name.clone()));
                                    close_menu();
                                }
                            },
                            div {
                                style: "width:11px;height:11px;flex:none;border-radius:50%;\
                                        box-shadow:inset 0 0 0 1px {choice_ring(current)};\
                                        display:grid;place-items:center",
                                if current {
                                    div {
                                        style: "width:5px;height:5px;border-radius:50%;\
                                                background-color:var(--color-accent)",
                                    }
                                }
                            }
                            span { style: "min-width:0;overflow:hidden;white-space:nowrap", "{name}" }
                            if current && drifted {
                                span {
                                    title: "Правила сдвинулись с этого пресета",
                                    style: "width:5px;height:5px;flex:none;border-radius:50%;\
                                            background-color:#a98ce8",
                                }
                            }
                        }
                    }
                }
            }

            div { class: "tray-sep" }
            span { class: "tray-head", "Системный прокси" }
            div { style: "display:flex;flex-direction:column;gap:2px",
                for mode in [ProxyMode::Unchanged, ProxyMode::Set, ProxyMode::Clear] {
                    div {
                        key: "{mode.key()}",
                        class: if mode == chosen { "act choice on" } else { "act choice" },
                        style: "display:flex;align-items:center;gap:9px;padding:5px 8px;\
                                border-radius:4px;font-size:13px;color:{choice_tint(mode == chosen)}",
                        onclick: {
                            let controller = controller.clone();
                            move |_| {
                                controller.send(Command::Proxy(mode));
                                close_menu();
                            }
                        },
                        div {
                            style: "width:11px;height:11px;flex:none;border-radius:50%;\
                                    box-shadow:inset 0 0 0 1px {choice_ring(mode == chosen)};\
                                    display:grid;place-items:center",
                            if mode == chosen {
                                div {
                                    style: "width:5px;height:5px;border-radius:50%;\
                                            background-color:var(--color-accent)",
                                }
                            }
                        }
                        "{mode.label()}"
                    }
                }
            }

            div { class: "tray-sep tray-foot" }
            div { style: "display:flex;gap:8px",
                div {
                    class: "act tray-act",
                    onclick: move |_| out_of_menu(&dioxus::desktop::window(), true),
                    "Открыть"
                }
                div {
                    class: "act tray-act",
                    onclick: move |_| quit(&dioxus::desktop::window()),
                    "Выход"
                }
            }
        }
    }
}

/// What the icon says when the pointer rests on it: the state, and the server
/// it is about. Not the rates — they change twice a second, these words are
/// read only while the pointer is on the icon, and every change is a call into
/// the shell.
fn tray_tooltip(frame: &Frame) -> String {
    match frame.server_tag.as_str() {
        "" => format!("bliss — {}", frame.state.label()),
        tag => format!("bliss — {} · {tag}", frame.state.label()),
    }
}

/// The frames of `assets/bliss.ico` that carry the mark alone. The file's two
/// larger frames add the wordmark, a smudge at any size a window icon is drawn.
const MARK_FRAMES: [u32; 13] = [16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 96];

/// The side to ask for, for an icon `at_100` px across at 100% scale.
///
/// Rounded down to a frame the file has. Given a side it has no frame for,
/// `LoadImageW` resizes the nearest one, and above 300% the nearest can be a
/// frame with the wordmark in it.
fn icon_side(at_100: f64, scale: f64) -> u32 {
    let want = (at_100 * scale).round() as u32;
    MARK_FRAMES
        .into_iter()
        .rev()
        .find(|&side| side <= want)
        .unwrap_or(MARK_FRAMES[0])
}

/// Sets the window's icons at the sizes a monitor of this scale draws.
///
/// The builder's copy only keeps Dioxus from installing its logo. It is loaded
/// before the event loop makes the process DPI-aware, at the default 32 px, and
/// set as the small icon with no large one; nobody sees it, because the window
/// is built hidden and this runs before it is shown. Called for the monitor the
/// window is built on, and by `WindowIcons` whenever the scale changes.
fn set_icons(window: &Window, scale: f64) {
    // The shell's small and large icons are 16 and 32 px at 100%.
    if let Some(small) = app_icon(Some(icon_side(16.0, scale))) {
        window.set_window_icon(Some(small));
    }
    #[cfg(windows)]
    if let Some(large) = app_icon(Some(icon_side(32.0, scale))) {
        use dioxus::desktop::tao::platform::windows::WindowExtWindows;
        window.set_taskbar_icon(Some(large));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Connection,
    Traffic,
    Subscriptions,
    Routing,
    Settings,
}

impl Screen {
    fn key(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Traffic => "traffic",
            Self::Subscriptions => "subscriptions",
            Self::Routing => "routing",
            Self::Settings => "settings",
        }
    }
}

/// The lower card on the connection screen holds three things, not one.
///
/// Tabs rather than three more sidebar destinations: all three are about the
/// server you are connected to right now, so they belong next to it. It also
/// fills a card that was a chart and a lot of nothing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Traffic,
    Servers,
    Actions,
}

impl Tab {
    fn key(self) -> &'static str {
        match self {
            Self::Traffic => "traffic",
            Self::Servers => "servers",
            Self::Actions => "actions",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Traffic => "Трафик",
            Self::Servers => "Серверы",
            Self::Actions => "Действия",
        }
    }
}

#[component]
fn App() -> Element {
    let controller = use_context::<Controller>();
    let mut frame = use_signal(|| controller.frame());
    let screen = use_signal(|| Screen::Connection);
    let tab = use_signal(|| Tab::Traffic);
    let window = dioxus::desktop::use_window();

    // The engine pushes; we never poll. A watch notification needs no timer, so
    // this works regardless of which executor is under the renderer.
    //
    // The controller is captured from the render scope rather than fetched
    // inside the closure: use_context is a hook, and hooks may only be called
    // while rendering.
    //
    // A frame nobody can see is not drawn. The window spends most of its life
    // in the tray, and drawing there cost as much as anything bliss did: every
    // frame rebuilt the page, and WebView2 laid it out and painted it into a
    // window that was not on the screen. The tray icon's tooltip still follows
    // (`TOOLTIP`), and whatever brings the window back draws the latest frame
    // first (`catch_up`); gone out of sight, WebView2 is told it may give
    // memory back (`rest`).
    let watcher = controller.clone();
    let watching = window.clone();
    use_future(move || {
        let mut controller = watcher.clone();
        let window = watching.clone();
        async move {
            loop {
                controller.changed().await;
                let words = controller.read(tray_tooltip);
                if *TOOLTIP.peek() != words {
                    *TOOLTIP.write() = words;
                }
                // The tray's own menu on Linux, which is wanted most while
                // the page gets no frames.
                #[cfg(target_os = "linux")]
                {
                    let model = controller.read(tray_linux::model);
                    if *tray_linux::MODEL.peek() != model {
                        *tray_linux::MODEL.write() = model;
                    }
                }
                if seen(&window) {
                    if BEHIND.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        wake(&window, &controller);
                    }
                    frame.set(controller.frame());
                } else if !BEHIND.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    rest(&window, true);
                    if TABLE_OPEN.load(std::sync::atomic::Ordering::Relaxed) {
                        controller.send(Command::Table(false));
                    }
                }
            }
        }
    });

    // See KEY_LISTENER.
    let keys = controller.clone();
    use_future(move || {
        let controller = keys.clone();
        async move {
            let mut listener = document::eval(KEY_LISTENER);
            while let Ok(key) = listener.recv::<String>().await {
                match key.as_str() {
                    // Whatever is there goes to the engine, which says what it
                    // made of it; an empty clipboard is an answer too.
                    "paste" => controller.send(Command::Import {
                        text: Link(crate::clipboard::paste().await.unwrap_or_default()),
                        from_form: false,
                    }),
                    "escape" => {
                        // The tray's menu is the outermost thing open, if it is.
                        if TRAY_MENU() {
                            out_of_menu(&dioxus::desktop::window(), false);
                        }
                        *ROW_MENU.write() = None;
                        *RULE_MENU.write() = false;
                        *DRAFT.write() = None;
                        *SUB_DRAFT.write() = None;
                    }
                    _ => {}
                }
            }
        }
    });

    // See CHART_HOVER.
    use_hook(|| {
        let _ = document::eval(CHART_HOVER);
    });

    // The window on Linux, now that the page is there: shown, unless it
    // starts in the tray, and minimized where there is no tray to start in
    // (`SHOW`, `ICONIFY`). Shown through tao, which shows what GTK packed
    // into it too; a window GTK alone shows stays empty.
    #[cfg(target_os = "linux")]
    {
        let shown = window.clone();
        use_effect(move || {
            if SHOW.swap(false, std::sync::atomic::Ordering::Relaxed) {
                shown.set_visible(true);
                if ICONIFY.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    shown.set_minimized(true);
                }
            }
        });
        let leaving = window.clone();
        use_hook(move || leave_on_signals(&leaving));
    }

    let live = frame;
    let frame = frame();

    rsx! {
        Styles {}
        WindowIcons {}
        WindowZoom {}
        Tray { frame: live }
        ToastLine { toast: frame.toast.clone() }
        if TRAY_MENU() {
            TrayMenu { frame: frame.clone() }
        }
        // The title bar spans the whole window, as in the reference, with the
        // sidebar beginning underneath it. Folded away by class while the tray
        // has the window: an inline `display` would be restored and stick.
        div {
            class: if TRAY_MENU() { "shell folded" } else { "shell" },
            // `display` is in `.shell`, not here: an inline property outranks
            // every class rule, and the tray's menu folds this away by class.
            style: "width:100vw;height:100vh;flex-direction:column;\
                    background-color:var(--color-bg);overflow:hidden;color:var(--color-text)",
            TitleBar { engine: frame.engine.clone() }
            div { style: "flex:1;min-height:0;display:flex",
                Sidebar { screen, tab, frame: frame.clone() }
                // The key changes with the screen, which forces a fresh node
                // and lets the fade-in replay instead of being skipped.
                {
                    let key = screen().key();
                    match screen() {
                        Screen::Connection => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Connection { frame: frame.clone(), tab }
                            }
                        },
                        Screen::Traffic => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Connections { frame: frame.clone() }
                            }
                        },
                        Screen::Subscriptions => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Subscriptions { frame: frame.clone() }
                            }
                        },
                        Screen::Routing => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Routing { frame: frame.clone() }
                            }
                        },
                        Screen::Settings => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Settings { frame: frame.clone() }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Both stylesheets, in a component of their own so they render once.
///
/// A component without props is never re-run by its parent. Inline in `App`,
/// which renders twice a second, every render copied the whole CSS — the font
/// and the picture inlined in it, about a hundred kilobytes — into fresh
/// strings only to find them unchanged.
#[component]
fn Styles() -> Element {
    rsx! {
        style { dangerous_inner_html: THEME_STYLES.as_str() }
        style { dangerous_inner_html: STYLES.as_str() }
    }
}

/// Picks the window's icons again whenever its monitor's scale changes.
///
/// `set_icons` chooses frames for the monitor the window is built on. Dragged
/// to a monitor with another scale, Windows would stretch or shrink those —
/// the very thing choosing by scale is for.
#[component]
fn WindowIcons() -> Element {
    let desktop = dioxus::desktop::window();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        if let Event::WindowEvent {
            window_id,
            event: WindowEvent::ScaleFactorChanged { scale_factor, .. },
            ..
        } = event
            && *window_id == desktop.window.id()
        {
            set_icons(&desktop.window, *scale_factor);
        }
    });
    rsx! {}
}

// --- chrome ---------------------------------------------------------------

#[component]
fn TitleBar(engine: String) -> Element {
    let window = dioxus::desktop::use_window();

    rsx! {
        div {
            style: "display:flex;align-items:center;height:38px;flex:none;padding-left:20px;\
                    flex-shrink:0",
            // The whole strip left of the buttons drags the window, which is
            // what a real title bar does and what the frame used to give us.
            div {
                style: "flex:1;height:38px;display:flex;align-items:center;gap:12px;cursor:default",
                onmousedown: {
                    let window = window.clone();
                    move |_| window.drag()
                },
                span {
                    style: "font-size:13px;letter-spacing:.14em;text-transform:uppercase;\
                            color:var(--color-neutral-600)",
                    "bliss"
                }
                if !engine.is_empty() {
                    span { style: "font-size:13px;color:var(--color-neutral-700)", "{engine}" }
                }
            }
            div { style: "display:flex;flex:none",
                WindowButton {
                    kind: "min",
                    onclick: {
                        let window = window.clone();
                        move |_| window.set_minimized(true)
                    },
                }
                WindowButton {
                    kind: "max",
                    onclick: {
                        let window = window.clone();
                        move |_| {
                            let maximized = window.is_maximized();
                            window.set_maximized(!maximized);
                        }
                    },
                }
                WindowButton {
                    kind: "close",
                    onclick: {
                        let window = window.clone();
                        move |_| window.close()
                    },
                }
            }
        }
    }
}

/// One of the three window controls.
///
/// 46x38 is the Windows metric and stays; only the mark inside grew. Drawn as
/// SVG rather than typed as a glyph, because the characters for these sit on a
/// text baseline and come out thin and off-centre at any size that fits the bar.
#[component]
fn WindowButton(kind: &'static str, onclick: EventHandler<MouseEvent>) -> Element {
    const SIZE: u32 = 17;
    let class = if kind == "close" {
        "wbtn wbtn-close"
    } else {
        "wbtn"
    };
    rsx! {
        div {
            class: "{class}",
            style: "width:46px;height:38px;display:grid;place-items:center;cursor:default",
            onclick: move |e| onclick.call(e),
            match kind {
                "min" => rsx! { crate::icons::Minimise { size: SIZE, color: MUTED } },
                "max" => rsx! { crate::icons::Maximise { size: SIZE, color: MUTED } },
                _ => rsx! { crate::icons::Close { size: SIZE, color: MUTED } },
            }
        }
    }
}

// --- sidebar --------------------------------------------------------------

/// Direction 1a's navigation: labels, not just icons. Six destinations is past
/// the point where an icon rail stays guessable.
#[component]
fn Sidebar(screen: Signal<Screen>, mut tab: Signal<Tab>, frame: Frame) -> Element {
    let here = screen();
    // Servers live in a tab of the connection screen rather than in a screen of
    // their own, so the rail points at the tab. Which means "Подключение" is
    // only the current place while that tab is not the one showing — otherwise
    // two rail items would light up for one view.
    let on_connection = here == Screen::Connection;
    let at_servers = on_connection && tab() == Tab::Servers;

    rsx! {
        div {
            // A card like the ones on the screens beside it, inset by the same
            // 18px at the top and bottom that the screens pad themselves with,
            // so its edges line up with theirs. 216px is what «Маршрутизация»
            // and a two-digit count need in Comfortaa at 14px.
            style: "width:216px;flex:none;display:flex;flex-direction:column;gap:2px;\
                    margin:18px 0 18px 20px;padding:10px;border-radius:8px;\
                    background-color:var(--color-surface);box-shadow:var(--shadow-sm)",

            NavItem {
                label: "Подключение",
                active: on_connection && !at_servers,
                enabled: true,
                onclick: move |_| {
                    screen.set(Screen::Connection);
                    if tab() == Tab::Servers {
                        tab.set(Tab::Traffic);
                    }
                },
                icon: rsx! {
                    crate::icons::Power { size: 18, color: nav_tint(on_connection && !at_servers, true) }
                },
            }
            NavItem {
                label: "Логи и трафик",
                active: here == Screen::Traffic,
                enabled: true,
                onclick: move |_| screen.set(Screen::Traffic),
                icon: rsx! {
                    crate::icons::Terminal { size: 18, color: nav_tint(here == Screen::Traffic, true) }
                },
            }
            NavItem {
                label: "Серверы",
                count: frame.servers.len(),
                active: at_servers,
                enabled: true,
                onclick: move |_| {
                    screen.set(Screen::Connection);
                    tab.set(Tab::Servers);
                },
                icon: rsx! { crate::icons::Globe { size: 18, color: nav_tint(at_servers, true) } },
            }

            NavItem {
                label: "Подписки",
                count: frame.subscriptions.len(),
                active: here == Screen::Subscriptions,
                enabled: true,
                onclick: move |_| screen.set(Screen::Subscriptions),
                icon: rsx! {
                    crate::icons::Rss { size: 18, color: nav_tint(here == Screen::Subscriptions, true) }
                },
            }
            NavItem {
                label: "Маршрутизация",
                count: frame.routing.rules.len(),
                active: here == Screen::Routing,
                enabled: true,
                onclick: move |_| screen.set(Screen::Routing),
                icon: rsx! {
                    crate::icons::Split { size: 18, color: nav_tint(here == Screen::Routing, true) }
                },
            }
            NavItem {
                label: "Настройки",
                active: here == Screen::Settings,
                enabled: true,
                onclick: move |_| screen.set(Screen::Settings),
                icon: rsx! {
                    crate::icons::Gear { size: 18, color: nav_tint(here == Screen::Settings, true) }
                },
            }

            // A cat asleep on the clouds, in the room the navigation leaves above
            // the engine: `.rail-cat` in app.css, the first thing to give way in a
            // short window.
            div { class: "rail-cat" }

            div {
                // A faint tint rather than the surface colour: the rail is that
                // colour now, and the box would vanish into it.
                style: "padding:12px;display:flex;flex-direction:column;gap:5px;\
                        border-radius:8px;background-color:rgba(239,232,245,.03)",
                span {
                    style: "font-size:12px;letter-spacing:.1em;text-transform:uppercase;\
                            color:var(--color-neutral-500)",
                    "Движок"
                }
                span {
                    style: "font-size:14px",
                    match (&frame.engine_setup, frame.engine.is_empty()) {
                        (crate::engine::EngineSetup::Installing { .. }, _) => "устанавливается…".to_owned(),
                        (crate::engine::EngineSetup::Missing { .. }, true) => "не установлен".to_owned(),
                        (_, true) => "не запущен".to_owned(),
                        (_, false) => frame.engine.clone(),
                    }
                }
                // Honest rather than aspirational: in stage 1 the engine really
                // is someone else's process running alongside us.
                span { style: "font-size:13px;color:var(--color-neutral-600)", "внешний процесс" }
            }
        }
    }
}

#[component]
fn NavItem(
    icon: Element,
    label: String,
    #[props(default = usize::MAX)] count: usize,
    active: bool,
    enabled: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    // State by class only. The inline style below is the same string for every
    // item in every state, and it has to stay that way: Dioxus restores any
    // inline property a new style string leaves out (see the note at the top of
    // this file), so an accent bar written inline for the active item was put
    // back on every item that had ever been clicked. The look for each state is
    // `.nav-item`, `.nav-item.active` and `.nav-item.disabled` in app.css.
    let class = match (active, enabled) {
        (true, _) => "nav-item active",
        (false, true) => "nav-item",
        (false, false) => "nav-item disabled",
    };
    rsx! {
        div {
            class: "{class}",
            style: "display:flex;align-items:center;gap:9px;padding:9px 10px;\
                    border-radius:8px;font-size:14px",
            onclick: move |e| if enabled { onclick.call(e) },
            {icon}
            "{label}"
            if count != usize::MAX {
                span {
                    style: "margin-left:auto;font-size:13px;color:var(--color-neutral-600);\
                            font-variant-numeric:tabular-nums",
                    "{count}"
                }
            }
        }
    }
}

/// Icons follow the label's colour, since Blitz will not inherit it for us.
fn nav_tint(active: bool, enabled: bool) -> &'static str {
    match (active, enabled) {
        (true, _) => ACCENT,
        (false, true) => "#b5a9c4",
        (false, false) => "#5a506a",
    }
}

// --- screen: connection ---------------------------------------------------

#[component]
fn Connection(frame: Frame, tab: Signal<Tab>) -> Element {
    let reason = frame.error.clone().unwrap_or_default();

    rsx! {
        div {
            style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px 18px 12px;overflow:hidden",

            div { style: "{SCREEN_HEAD}",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Подключение" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;overflow:hidden;white-space:nowrap",
                    "{elide(&frame.server_tag, 40)}"
                }
                if !frame.routing.presets.is_empty() {
                    PresetPills { view: frame.routing.clone() }
                }
                if frame.vpn.name.is_some() {
                    VpnChip { vpn: frame.vpn.clone(), alone: frame.routing.presets.is_empty() }
                }
            }

            // The reason a start failed. The engine has always carried it in the
            // frame and the window never read it, so a failure was a red ring
            // saying "Ошибка" and nothing else — the explanation existed only in
            // the log, which is one screen away and not where anyone looks first.
            if !reason.is_empty() {
                div {
                    style: "flex:none;display:flex;flex-direction:column;gap:9px;\
                            padding:12px;border-radius:8px;\
                            background-color:rgba(229,107,111,.10);\
                            box-shadow:inset 0 0 0 1px rgba(229,107,111,.26)",
                    span {
                        style: "font-size:13.5px;line-height:1.45;color:#f2a7ad;\
                                min-width:0;word-break:break-word",
                        "{reason}"
                    }
                    EngineInstall { setup: frame.engine_setup.clone() }
                }
            }

            // Before the first server there is no status to show, so the card
            // that would hold it asks for a server instead.
            if frame.servers.is_empty() {
                AddServerCard { frame: frame.clone() }
            } else {
                StatusCards { frame: frame.clone() }
            }

            div {
                style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:11px;\
                        padding:16px;background-color:var(--color-surface);\
                        border-radius:8px;box-shadow:var(--shadow-sm)",

                TabBar {
                    tab,
                    meta: match tab() {
                        // The rates alone: the axis under the chart already
                        // says it spans the last seconds. Said here too, the
                        // line outgrew the room beside the tabs in the narrowest
                        // window once traffic reached megabytes, and the bar
                        // would hide it and show it again as the numbers moved.
                        Tab::Traffic => rsx! {
                            div {
                                style: "display:flex;align-items:baseline;gap:16px;font-size:13.5px;\
                                        font-variant-numeric:tabular-nums",
                                span { style: "color:var(--color-accent-300)", "↓ {rate(frame.down_rate)}" }
                                span { style: "color:var(--color-neutral-400)", "↑ {rate(frame.up_rate)}" }
                            }
                        },
                        // Says what the number is before anyone reads it as a
                        // promise the tunnel will be this fast.
                        Tab::Servers => rsx! {
                            span { style: "font-size:13px;color:var(--color-neutral-600)",
                                "отклик — TCP-рукопожатие до сервера, мимо туннеля"
                            }
                        },
                        Tab::Actions => rsx! {},
                    },
                }

                // Keyed so switching tabs replays the fade rather than swapping
                // the contents in place.
                div {
                    key: "{tab().key()}",
                    class: "screen",
                    style: "flex:1;min-height:0;display:flex;flex-direction:column",
                    match tab() {
                        Tab::Traffic => rsx! { TrafficTab { frame: frame.clone() } },
                        Tab::Servers => rsx! { ServersTab { frame: frame.clone() } },
                        Tab::Actions => rsx! { ActionsTab { frame: frame.clone() } },
                    }
                }
            }
        }
    }
}

/// What the window can do about a missing engine: fetch it, and show how far
/// that has got. It used to name a console command and nothing else — run
/// from PowerShell, a command of this windowed program gives the prompt back
/// at once and prints later — and a window waiting for a click to look again.
#[component]
fn EngineInstall(setup: crate::engine::EngineSetup) -> Element {
    use crate::engine::EngineSetup;
    let controller = use_context::<Controller>();
    match setup {
        EngineSetup::Present => rsx! {},
        EngineSetup::Missing { problem } => rsx! {
            div { style: "display:flex;align-items:center;flex-wrap:wrap;gap:8px 12px",
                Button {
                    label: "Установить движок",
                    enabled: true,
                    onclick: move |_| controller.send(Command::InstallEngine),
                    icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                }
                span { style: "font-size:12.5px;line-height:1.45;color:var(--color-neutral-500)",
                    "xray-core с GitHub, около 15 МБ; подключусь сразу после"
                }
                if let Some(problem) = problem {
                    span {
                        style: "flex-basis:100%;font-size:12.5px;line-height:1.45;color:#f2a7ad;\
                                word-break:break-word",
                        "Не вышло: {problem}"
                    }
                }
            }
        },
        EngineSetup::Installing { stage, percent } => rsx! {
            div { style: "display:flex;flex-direction:column;gap:6px;max-width:420px",
                span { style: "font-size:13px;color:var(--color-accent-2-300)",
                    match percent {
                        Some(percent) => format!("{stage} — {percent}%"),
                        None => format!("{stage}…"),
                    }
                }
                div {
                    style: "height:5px;border-radius:3px;overflow:hidden;\
                            background-color:rgba(239,232,245,.08)",
                    div {
                        style: "height:100%;width:{percent.unwrap_or(0)}%;border-radius:3px;\
                                background-color:#a98ce8",
                    }
                }
            }
        },
    }
}

/// The status card and the numbers beside it, for a window with a server.
#[component]
fn StatusCards(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let (tone, outer, inner) = tones(frame.state);
    let on = frame.state.is_on();

    rsx! {
        div { style: "display:flex;gap:13px;align-items:stretch;flex:none",
            div {
                // The night scene behind the status: `.status-card` in app.css.
                class: "status-card",
                style: "flex:1;min-width:0;display:flex;align-items:center;gap:18px;\
                        padding:16px;background-color:var(--color-surface);\
                        border-radius:8px;box-shadow:var(--shadow-sm)",
                div {
                    style: "flex:none;display:flex;flex-direction:column;align-items:center;gap:11px",
                    div {
                        class: "ring",
                        style: "width:78px;height:78px;flex:none;border-radius:50%;display:grid;\
                                place-items:center;border:1px solid {tone};\
                                box-shadow:0 0 34px {outer}, inset 0 0 20px {inner}",
                        title: if on { "Отключить" } else { "Подключить" },
                        onclick: move |_| controller.send(Command::Toggle { was_on: on }),
                        crate::icons::Heart { size: 28, color: tone, filled: on }
                    }
                    TunSwitch { tun: frame.tun.clone() }
                }
                    // Clips on a narrow card instead of running under the switch beside it.
                    div { style: "display:flex;flex-direction:column;gap:4px;min-width:0;overflow:hidden",
                    span {
                        // Longhands, not the `font:` shorthand: see the
                        // note at the top of this file.
                        style: "font-weight:700;font-size:25px;line-height:1.1;\
                                font-family:var(--font-heading);color:{tone}",
                        "{frame.state.label()}"
                    }
                    span { style: "font-size:14px;color:var(--color-neutral-400)",
                        // A server without TLS has no name to show, and a line
                        // opening on a separator looks like something fell off.
                        if frame.sni.is_empty() {
                            "{frame.server_addr}"
                        } else {
                            "{elide(&frame.sni, 26)} · {frame.server_addr}"
                        }
                    }
                    span {
                        // One line: `VLESS · xhttp · reality · vision` is long,
                        // and a narrow card clips its tail rather than growing.
                        style: "font-size:14px;color:var(--color-neutral-500);white-space:nowrap",
                        if on {
                            // Boxed digits, or the tail of this line walks left
                            // and right once a second; see `FIGURE_EM`.
                            span { "В сети " }
                            Ticking { text: frame.uptime.clone() }
                            span { " · {frame.flow}" }
                        } else {
                            "{frame.flow}"
                        }
                    }
                }
                // One switch: the ring. A pill switch beside it did the same
                // thing, and two controls for one action read as two actions.
                // `.listen` in app.css: hidden in a narrow window.
                span {
                    class: "listen",
                    style: "margin-left:auto;align-self:flex-start;font-size:13px;\
                            color:var(--color-neutral-600);white-space:nowrap",
                    "{frame.listen}"
                }
            }

            div {
                // Its width is `.stats-card` in app.css, which narrows it in a
                // narrow window so the state word beside it still fits.
                class: "stats-card",
                style: "flex:none;display:flex;flex-direction:column;gap:10px;\
                        padding:16px;background-color:var(--color-surface);\
                        border-radius:8px;box-shadow:var(--shadow-sm)",
                StatRow {
                    label: "Задержка",
                    value: match frame.latency_ms {
                        Some(ms) => format!("{ms} мс"),
                        None => "—".to_owned(),
                    },
                }
                StatRow { label: "Сессия ↓", value: bytes(frame.down_total) }
                StatRow { label: "Сессия ↑", value: bytes(frame.up_total) }
                StatRow { label: "Соединений", value: frame.active.to_string() }
                StatRow { label: "uTLS", value: frame.fingerprint.clone() }
            }
        }
    }
}

/// What goes through bliss: only what is pointed at its ports, or everything.
///
/// Two positions with a knob that slides between them, under the ring, because
/// it is the second thing about the tunnel a person decides and the first place
/// they look. How each position looks is `.tun-switch` in app.css — the inline
/// style here is one constant string, and only the class and the icons' colour
/// change with the state (the note at the top of this file).
///
/// The right-hand side is where it is *asked* to be, not where it got to: a
/// copy of bliss without administrator rights cannot make an adapter, so the
/// knob moves and the colour says "on its way" rather than "done". The click
/// that could not take effect says why in a line at the bottom of the window.
#[component]
fn TunSwitch(tun: crate::engine::TunView) -> Element {
    let controller = use_context::<Controller>();
    let class = match (tun.on, tun.carrying) {
        (false, _) => "tun-switch",
        (true, true) => "tun-switch on",
        (true, false) => "tun-switch on waiting",
    };
    let (ports, machine) = match (tun.on, tun.carrying) {
        (false, _) => (ACCENT, MUTED),
        (true, true) => (MUTED, ACCENT),
        (true, false) => (MUTED, BUSY.0),
    };
    let title = match (tun.on, tun.carrying, tun.elevated) {
        (false, ..) => "Через bliss идёт только то, что направили в его порты. \
             Включить TUN — весь трафик машины, включая UDP: звонки, игры, QUIC"
            .to_owned(),
        (true, true, _) => "Весь трафик машины идёт через bliss. Выключить".to_owned(),
        (true, false, false) => format!(
            "TUN включён, но адаптеру не хватает прав: {}",
            bliss_tun::rights_hint()
        ),
        (true, false, true) => "TUN включён, но адаптер не поднялся".to_owned(),
    };

    rsx! {
        div {
            class: "{class}",
            style: "flex:none;display:flex;align-items:center",
            title,
            onclick: {
                let on = tun.on;
                move |_| controller.send(Command::Tun(!on))
            },
            div { class: "tun-knob" }
            div { class: "tun-side",
                crate::icons::Plug { size: 15, color: ports }
            }
            div { class: "tun-side",
                crate::icons::Machine { size: 15, color: machine }
            }
        }
    }
}

/// Where the VPN's connection is, in a few words that start small: for the
/// pointer over the main screen's chip and the line under the settings
/// screen's switch.
fn vpn_line(vpn: &crate::engine::VpnView) -> String {
    use crate::engine::VpnState;
    match &vpn.state {
        VpnState::Off => "выключен".to_owned(),
        VpnState::Connecting(step) => format!("подключаюсь — {step}…"),
        VpnState::Up(address) => format!("подключён, адрес в рабочей сети {address}"),
        VpnState::Failed(why) => format!("не подключился: {why}"),
    }
}

/// The first letter as a capital, for a line that starts a sentence.
fn capitalized(text: &str) -> String {
    let mut letters = text.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => String::new(),
    }
}

/// The VPN's switch on the main screen: a chip in the heading, beside the
/// presets, whose dot says where the connection is. A click turns it on or
/// off; the settings screen has the rest. How each state looks is
/// `.vpn-chip` in app.css, by class (the note at the top of this file).
#[component]
fn VpnChip(vpn: crate::engine::VpnView, alone: bool) -> Element {
    use crate::engine::VpnState;
    let controller = use_context::<Controller>();
    let state = match &vpn.state {
        VpnState::Up(_) => "up",
        VpnState::Connecting(_) => "busy",
        VpnState::Failed(_) => "failed",
        VpnState::Off => "off",
    };
    let class = if alone {
        format!("vpn-chip {state} alone")
    } else {
        format!("vpn-chip {state}")
    };
    let name = vpn.name.clone().unwrap_or_default();
    let on = vpn.on;
    let title = format!(
        "Рабочий VPN «{name}»: {}. Нажмите, чтобы {}",
        vpn_line(&vpn),
        if on {
            "отключить"
        } else {
            "подключить"
        }
    );
    rsx! {
        span {
            class: "{class}",
            title: "{title}",
            onclick: move |_| controller.send(Command::Vpn(!on)),
            span { class: "vpn-dot" }
            "VPN"
        }
    }
}

/// The settings screen's card for the VPN beside the tunnel: the profile the
/// work network handed out, its switch, and what it is doing.
#[component]
fn VpnCard(vpn: crate::engine::VpnView) -> Element {
    use bliss_openvpn::Availability;
    let controller = use_context::<Controller>();
    // Deleting takes the profile with it, and getting it back means finding
    // the file again: a second click says yes.
    let mut sure = use_signal(|| false);
    let service = match vpn.service {
        Availability::Ready => None,
        Availability::Stopped if cfg!(windows) => Some(
            "Служба OpenVPN остановлена: запустите «OpenVPN Interactive Service» в службах \
             Windows.",
        ),
        // On Linux: OpenVPN is there, and nothing can start it as root.
        Availability::Stopped => Some(
            "Нет pkexec (polkit): без него bliss не может запустить OpenVPN с правами \
             администратора. Установите пакет polkit.",
        ),
        Availability::Missing if cfg!(windows) => Some(
            "Нужен OpenVPN: установите OpenVPN Community — openvpn.net, раздел Community \
             Downloads. Его служба даёт bliss подключаться без прав администратора.",
        ),
        Availability::Missing => Some(
            "Нужен OpenVPN: установите пакет openvpn (например, sudo apt install openvpn). \
             При подключении система спросит пароль администратора.",
        ),
    };
    let failed = match &vpn.state {
        crate::engine::VpnState::Failed(why) => Some(why.clone()),
        _ => None,
    };
    let on = vpn.on;

    rsx! {
        div {
            style: "flex:none;display:flex;flex-direction:column;gap:4px;padding:16px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            span { style: "font:700 16px/1 var(--font-heading);padding:0 0 8px", "Рабочий VPN" }
            span {
                style: "padding:0 0 6px;font-size:13px;line-height:1.5;color:var(--color-neutral-500)",
                "Для рабочих сайтов, по профилю OpenVPN (.ovpn), который выдали на работе. \
                 Через VPN идёт только то, что правила отправляют «через VPN»: остальной \
                 трафик и DNS остаются как были."
            }
            if let Some(note) = service {
                span {
                    style: "padding:2px 0 6px;font-size:12.5px;line-height:1.45;color:#cbbaf4",
                    "{note}"
                }
            }
            if let Some(name) = vpn.name.clone() {
                CheckRow {
                    checked: on,
                    enabled: true,
                    title: format!("Подключать «{name}»"),
                    hint: if failed.is_some() {
                        "Не подключился.".to_owned()
                    } else {
                        format!("{}.", capitalized(&vpn_line(&vpn)))
                    },
                    onclick: {
                        let controller = controller.clone();
                        move |_| controller.send(Command::Vpn(!on))
                    },
                }
                // Without the service every start fails the same way, and
                // the note above says so already.
                if let Some(why) = failed.filter(|_| vpn.service == Availability::Ready) {
                    span {
                        style: "padding:2px 0 4px 29px;font-size:12.5px;line-height:1.45;\
                                color:#f2a7ad;word-break:break-word",
                        "{why}"
                    }
                }
                div { style: "display:flex;flex-wrap:wrap;gap:8px;padding:6px 0 2px",
                    OvpnPicker { label: "Заменить файл" }
                    Button {
                        label: if sure() { "Точно удалить?" } else { "Удалить" },
                        enabled: true,
                        onclick: {
                            let controller = controller.clone();
                            move |_| {
                                if sure() {
                                    sure.set(false);
                                    controller.send(Command::ForgetVpn);
                                } else {
                                    sure.set(true);
                                }
                            }
                        },
                        icon: rsx! { crate::icons::Trash { size: 15, color: if sure() { BAD } else { MUTED } } },
                    }
                }
            } else {
                div {
                    style: "display:flex;align-items:center;flex-wrap:wrap;gap:8px 12px;padding:6px 0 2px",
                    OvpnPicker { label: "Выбрать файл .ovpn" }
                    span { style: "font-size:12.5px;color:var(--color-neutral-600)",
                        "или скопируйте текст файла и нажмите Ctrl+V"
                    }
                }
            }
        }
    }
}

/// A button that opens Windows' own file dialog for a profile and hands the
/// file to the engine. Dioxus opens the dialog for the hidden input; the
/// label around it is what a person clicks.
#[component]
fn OvpnPicker(label: String) -> Element {
    let controller = use_context::<Controller>();
    rsx! {
        label {
            class: "act",
            style: "display:inline-flex;align-items:center;gap:7px;padding:6px 13px;border-radius:4px;\
                    font-size:14px;white-space:nowrap;flex:none;color:var(--color-neutral-200);\
                    box-shadow:inset 0 0 0 1px rgba(238,130,196,.35)",
            input {
                r#type: "file",
                accept: ".ovpn,.conf",
                style: "display:none",
                onchange: move |e: FormEvent| {
                    let controller = controller.clone();
                    async move {
                        let Some(file) = e.files().into_iter().next() else {
                            return;
                        };
                        // Measured before it is read: a cap checked after the
                        // allocation is not a cap.
                        if file.size() > bliss_openvpn::MAX_PROFILE as u64 {
                            controller.send(Command::Say {
                                text: format!(
                                    "«{}» не похож на профиль OpenVPN: он больше {} КБ",
                                    file.name(),
                                    bliss_openvpn::MAX_PROFILE / 1024
                                ),
                                bad: true,
                            });
                            return;
                        }
                        match file.read_string().await {
                            Ok(text) => controller.send(Command::AddVpn {
                                name: file.name(),
                                profile: Secret(text),
                            }),
                            Err(e) => controller.send(Command::Say {
                                text: format!("файл не прочитался: {e}"),
                                bad: true,
                            }),
                        }
                    }
                },
            }
            crate::icons::Plus { size: 15, color: ACCENT }
            "{label}"
        }
    }
}

/// What the connection screen shows before there is a server: the one thing
/// bliss needs from anyone, asked for in the window rather than in a file.
#[component]
fn AddServerCard(frame: Frame) -> Element {
    rsx! {
        div {
            // The same night scene as the status card it stands in for.
            class: "status-card",
            style: "flex:none;display:flex;flex-direction:column;gap:14px;padding:16px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            div { style: "display:flex;flex-direction:column;gap:6px;max-width:540px",
                span {
                    style: "font-weight:700;font-size:23px;line-height:1.15;\
                            font-family:var(--font-heading);color:var(--color-text)",
                    "Добавьте сервер"
                }
                span { style: "font-size:14px;line-height:1.5;color:var(--color-neutral-400)",
                    "Вставьте ссылку на сервер — vless://, vmess://, trojan://, ss://, \
                     hysteria2:// или wireguard:// — или адрес подписки: их даёт провайдер. \
                     bliss сохранит их в профиль и подключится. Можно просто нажать Ctrl+V."
                }
            }
            ImportForm { frame, placeholder: "vless://, trojan://, ss://… или https://… подписки" }
        }
    }
}

/// Paste a link, a list of them or a subscription's address; get servers.
///
/// The typed text lives in a local signal rather than in the frame, which is
/// republished twice a second and would fight whoever is typing. The engine's
/// answer comes back in `frame.added`; its `attempt` tells a fresh answer from
/// the one already on screen when this form appeared, so switching tabs does
/// not replay an old "added".
#[component]
fn ImportForm(
    frame: Frame,
    placeholder: &'static str,
    /// Across its card rather than at most 680px wide: over cards with buttons
    /// of their own, so its buttons end where theirs do.
    #[props(default)]
    wide: bool,
) -> Element {
    let controller = use_context::<Controller>();
    let mut link = use_signal(String::new);
    let seen = use_hook(|| frame.added.as_ref().map(|added| added.attempt));
    let fresh = frame
        .added
        .clone()
        .filter(|added| Some(added.attempt) != seen);

    // Empty the field when a fresh success arrives, once. In an effect: writing
    // a signal while rendering would only render again for nothing.
    let succeeded = fresh
        .as_ref()
        .filter(|added| added.outcome.is_ok())
        .map(|added| added.attempt);
    use_effect(use_reactive((&succeeded,), move |(succeeded,)| {
        if succeeded.is_some() {
            link.set(String::new());
        }
    }));

    let submit = move |text: String| {
        if !text.trim().is_empty() {
            controller.send(Command::Import {
                text: Link(text),
                from_form: true,
            });
        }
    };
    let typed = !link().trim().is_empty();
    let width = if wide { "none" } else { "680px" };

    rsx! {
        div { style: "display:flex;flex-direction:column;gap:7px;max-width:{width}",
            div { style: "display:flex;align-items:center;gap:8px",
                input {
                    class: "field",
                    r#type: "text",
                    value: "{link}",
                    placeholder: "{placeholder}",
                    autocomplete: "off",
                    spellcheck: "false",
                    style: "flex:1;min-width:0;padding:8px 10px;border-radius:4px;border:none;\
                            color:var(--color-text);font-size:14px;\
                            font-family:ui-monospace,Consolas,monospace",
                    oninput: move |e| link.set(e.value()),
                    onkeydown: {
                        let submit = submit.clone();
                        move |e: KeyboardEvent| {
                            if e.key() == Key::Enter {
                                submit(link());
                            }
                        }
                    },
                }
                Button {
                    label: "Добавить",
                    enabled: typed,
                    onclick: {
                        let submit = submit.clone();
                        move |_| submit(link())
                    },
                    icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                }
                // Straight from the clipboard, in one click: the link is long,
                // and selecting a field to paste into is one more thing to aim at.
                Button {
                    label: "Вставить",
                    enabled: true,
                    // A clone like the others: rsx builds this handler before
                    // theirs, so moving the original here left them nothing.
                    onclick: {
                        let submit = submit.clone();
                        move |_| {
                            let submit = submit.clone();
                            spawn(async move {
                                if let Some(text) = crate::clipboard::paste().await {
                                    let text = text.trim().to_owned();
                                    link.set(text.clone());
                                    submit(text);
                                }
                            });
                        }
                    },
                    icon: rsx! { crate::icons::Copy { size: 15, color: ACCENT } },
                }
            }
            match fresh.map(|added| added.outcome) {
                Some(Ok(done)) => rsx! {
                    span { style: "font-size:13px;color:#72cf9e", "{elide(&done, 90)}" }
                },
                Some(Err(why)) => rsx! {
                    span {
                        style: "font-size:13px;line-height:1.45;color:#f2a7ad;word-break:break-word",
                        "{why}"
                    }
                },
                None => rsx! {},
            }
        }
    }
}

// --- the tabbed card ------------------------------------------------------

#[component]
fn TabBar(mut tab: Signal<Tab>, meta: Element) -> Element {
    rsx! {
        // One line, of one height under every tab. What a tab shows beside the
        // tabs fits on that line whole or wraps onto a second one, which the
        // bar cuts off. The servers' hint used to wrap in place in a window
        // under 1000px wide: the bar grew under that tab alone, and the tabs
        // and the card below them moved 10px on every switch.
        div {
            style: "flex:none;height:30px;overflow:hidden;display:flex;flex-wrap:wrap;\
                    align-items:center;align-content:flex-start;gap:6px",
            for one in [Tab::Traffic, Tab::Servers, Tab::Actions] {
                div {
                    key: "{one.key()}",
                    class: if tab() == one { "tab active" } else { "tab" },
                    style: "flex:none;padding:5px 12px;border-radius:4px;\
                            font:700 14.5px/1.35 var(--font-heading)",
                    onclick: move |_| tab.set(one),
                    "{one.label()}"
                }
            }
            div { style: "margin-left:auto;white-space:nowrap", {meta} }
        }
    }
}

/// Download rates over the last twenty seconds, as a line that glides.
///
/// The paths are drawn in the chart's own units, `CHART_STEP` apart and one
/// KB/s to a unit up, so they do not change when only the range does: the
/// group around them carries the range as a transform, which eases
/// (`chart_lift`). The layer glides one step left per tick and starts again on
/// each new sample (`glide_class`); the chart's section in app.css says why
/// each part is there, and `CHART_HOVER` what the pointer reads off it.
#[component]
fn TrafficTab(frame: Frame) -> Element {
    // Two of the samples are past the chart's edges for most of a tick.
    let intervals = frame.history.len().saturating_sub(2) as u128;
    let span_seconds = intervals * crate::engine::TICK.as_millis() / 1000;
    let samples = frame.samples;
    // The sample the tab opened on: until the next one there is no glide in
    // step with the ticks to join.
    let opened = use_hook(move || samples);
    let idle = frame.history.iter().all(|&rate| rate == 0);
    let glide = glide_class(samples, opened, idle);
    let (line, area) = chart_paths(&frame.history);
    let width = CHART_STEP * frame.history.len().saturating_sub(1) as f64;
    let group = format!(
        "transform:translate(0px,{CHART_HEIGHT}px) scale(1,-{:.8})",
        chart_lift(frame.scale)
    );
    let stroke = if idle { CHART_IDLE } else { ACCENT };
    // For the readout under the pointer: each sample's rate, and as the tip
    // writes it.
    let rates = frame
        .history
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let labels = frame
        .history
        .iter()
        .map(|&sample| format!("↓ {}", rate(sample)))
        .collect::<Vec<_>>()
        .join("|");
    rsx! {
        div { style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:9px",
            div { class: "chart", "data-rates": "{rates}", "data-labels": "{labels}",
                div { class: glide,
                    svg {
                        width: "100%",
                        height: "100%",
                        view_box: "0 0 {width} {CHART_HEIGHT}",
                        preserve_aspect_ratio: "none",
                        defs {
                            linearGradient {
                                id: "chart-fill",
                                x1: "0",
                                y1: "1",
                                x2: "0",
                                y2: "0",
                                stop { offset: "0", stop_color: ACCENT, stop_opacity: "0.3" }
                                stop { offset: "1", stop_color: ACCENT, stop_opacity: "0.02" }
                            }
                        }
                        g { class: "chart-scale", style: "{group}",
                            path { d: "{area}", fill: "url(#chart-fill)" }
                            path {
                                d: "{line}",
                                fill: "none",
                                stroke,
                                stroke_width: "1.8",
                                stroke_linejoin: "round",
                                vector_effect: "non-scaling-stroke",
                            }
                        }
                    }
                }
                div { class: "chart-guide" }
                div { class: "chart-dot" }
                div { class: "chart-tip" }
            }
            div {
                style: "display:flex;justify-content:space-between;font-size:12px;\
                        color:var(--color-neutral-600);font-variant-numeric:tabular-nums",
                span { "−{span_seconds} с" }
                span { "−{span_seconds / 2} с" }
                span { "сейчас" }
            }
        }
    }
}

/// Every server in the profile, with the one in use marked and the rest a
/// click away.
///
/// Switching restarts the engine, because its config names one server and
/// there is nothing to reload — so the row does not pretend to be instant.
#[component]
fn ServersTab(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let probing = sweeping(&frame);
    // A delete clicked once, by the row's place in the list; the second deletes.
    let armed = use_signal(|| None::<usize>);
    let sections = server_sections(&frame.servers, &GROUP_OPEN());
    let columns = ServerColumns::new(
        SERVER_LIST_WIDTH(),
        SERVER_NAME_WIDTH(),
        SERVER_KIND_WIDTH(),
    );
    let grid = columns.map_or_else(|| SERVER_COLUMNS.to_owned(), |c| c.css());
    // Where a drag began: which border, the pointer's place, the column's width.
    let mut drag = use_signal(|| None::<(ColumnBorder, f64, f64)>);

    rsx! {
        div { style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:7px",
            div {
                class: "list-head",
                style: "display:grid;{grid};position:relative;padding:0 10px 6px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);\
                        border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                // The content box: the padding and the scrollbar's gutter are
                // left out, as the rows below have them.
                onresize: move |e: ResizeEvent| {
                    if let Ok(size) = e.get_content_box_size() {
                        let width = size.width + SERVER_PAD * 2.0;
                        if (SERVER_LIST_WIDTH() - width).abs() >= 1.0 {
                            *SERVER_LIST_WIDTH.write() = width;
                        }
                    }
                },
                if let Some(c) = columns {
                    // Each grip is centred on the gap it stands in.
                    for (border, left, start) in [
                        (ColumnBorder::Name, SERVER_PAD + c.name, c.name),
                        (ColumnBorder::Kind, SERVER_PAD + c.name + 12.0 + c.addr, c.kind),
                    ] {
                        div {
                            key: "{border:?}",
                            class: if drag().is_some_and(|(b, _, _)| b == border) { "col-grip dragging" } else { "col-grip" },
                            style: "left:{left}px",
                            title: "Потяните, чтобы изменить ширину. Двойной клик — как было",
                            onmousedown: move |e: MouseEvent| {
                                e.prevent_default();
                                // The name holds still while the protocol's
                                // border moves: the address gives and takes.
                                if border == ColumnBorder::Kind && SERVER_NAME_WIDTH().is_none() {
                                    *SERVER_NAME_WIDTH.write() = Some(c.name);
                                }
                                drag.set(Some((border, e.client_coordinates().x, start)));
                            },
                            ondoubleclick: move |_| match border {
                                ColumnBorder::Name => *SERVER_NAME_WIDTH.write() = None,
                                ColumnBorder::Kind => *SERVER_KIND_WIDTH.write() = None,
                            },
                        }
                    }
                }
                span { "Имя" }
                span { "Адрес" }
                span { "Протокол" }
                // The hint beside the tabs says it too, and leaves a narrow window.
                span {
                    style: "text-align:right",
                    title: "TCP-рукопожатие до сервера, мимо туннеля",
                    "Отклик"
                }
                span {}
            }
            // Over the whole window while dragging, as the log's bar has it.
            if let Some((border, from, start)) = drag() {
                div {
                    class: "drag-veil cols",
                    onmousemove: move |e: MouseEvent| {
                        use dioxus::prelude::dioxus_elements::input_data::MouseButton;
                        if !e.held_buttons().contains(MouseButton::Primary) {
                            drag.set(None);
                            return;
                        }
                        let moved = e.client_coordinates().x - from;
                        let width = SERVER_LIST_WIDTH();
                        // Stored as the columns will draw it, so a drag past
                        // a limit does not leave a stretch that moves nothing
                        // on the way back.
                        match border {
                            ColumnBorder::Name => {
                                let kind = SERVER_KIND_WIDTH();
                                if let Some(c) = ServerColumns::new(width, Some(start + moved), kind) {
                                    *SERVER_NAME_WIDTH.write() = Some(c.name);
                                }
                            }
                            ColumnBorder::Kind => {
                                let name = SERVER_NAME_WIDTH();
                                if let Some(c) = ServerColumns::new(width, name, Some(start - moved)) {
                                    *SERVER_KIND_WIDTH.write() = Some(c.kind);
                                }
                            }
                        }
                    },
                    onmouseup: move |_| drag.set(None),
                }
            }

            div { style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden",
                if frame.servers.is_empty() {
                    div { style: "padding:22px 10px;font-size:14.5px;color:var(--color-neutral-600)",
                        "Серверов пока нет — вставьте ссылку на сервер в поле ниже или нажмите Ctrl+V."
                    }
                }
                for section in sections {
                    // A subscription's servers sit under its name, closed
                    // unless the server in use is among them.
                    if let Some((id, name)) = section.group.clone() {
                        div {
                            key: "head-{id}",
                            class: "group-head",
                            style: "display:flex;align-items:center;gap:9px;margin-top:4px;\
                                    padding:9px 10px;border-radius:4px",
                            onclick: {
                                let open = section.open;
                                move |_| {
                                    GROUP_OPEN.write().insert(id.clone(), !open);
                                }
                            },
                            if section.open {
                                crate::icons::ChevronDown { size: 15, color: MUTED }
                            } else {
                                crate::icons::ChevronRight { size: 15, color: MUTED }
                            }
                            crate::icons::Rss { size: 14, color: ACCENT }
                            span {
                                style: "font-size:14px;font-weight:700;min-width:0;overflow:hidden;\
                                        white-space:nowrap",
                                "{elide(&name, 34)}"
                            }
                            span {
                                style: "font-size:12.5px;color:var(--color-neutral-600);white-space:nowrap",
                                "{plural(section.rows.len() as u64, \"сервер\", \"сервера\", \"серверов\")}"
                            }
                            if !section.open && section.has_current {
                                span {
                                    style: "margin-left:auto;font-size:12.5px;white-space:nowrap;\
                                            color:var(--color-accent-300)",
                                    "здесь текущий сервер"
                                }
                            }
                        }
                    }
                    if section.open {
                        for index in section.rows.iter().copied() {
                            ServerLine {
                                key: "{index}",
                                index,
                                server: frame.servers[index].clone(),
                                grouped: section.group.is_some(),
                                armed,
                            }
                        }
                    }
                }
            }

            ImportForm { frame: frame.clone(), placeholder: "vless://, trojan://, ss://… или адрес подписки" }

            div { style: "display:flex;align-items:center;gap:10px;padding-top:2px",
                Button {
                    label: if probing { "Проверяю…" } else { "Проверить все" },
                    enabled: !probing && !frame.servers.is_empty(),
                    onclick: {
                        let controller = controller.clone();
                        move |_| controller.send(Command::PingAll)
                    },
                    icon: rsx! { crate::icons::Pulse { size: 15, color: ACCENT } },
                }
                span { style: "font-size:13px;color:var(--color-neutral-600)",
                    "переключение перезапускает движок"
                }
            }
        }
    }
}

/// The things one actually does with a client, none of which had a home.
#[component]
fn ActionsTab(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let on = frame.state.is_on();
    // The same guard the list's own button carries. Without it the two buttons
    // for one command disagreed, and this one could start sweep after sweep.
    let probing = sweeping(&frame);
    let refreshing = frame.subscriptions.iter().all(|s| s.fetching);

    rsx! {
        div {
            // As many columns as fit: two in the default window, more when it
            // grows, rather than two cards stretched across a wide screen.
            style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;display:grid;\
                    grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:9px;\
                    align-content:start;padding-top:2px",

            ActionCard {
                title: if probing { "Проверяю серверы…" } else { "Проверить серверы" },
                hint: "рукопожатие до каждого из профиля",
                enabled: !probing && !frame.servers.is_empty(),
                onclick: {
                    let controller = controller.clone();
                    move |_| controller.send(Command::PingAll)
                },
                icon: rsx! { crate::icons::Pulse { size: 17, color: ACCENT } },
            }
            ActionCard {
                title: "Перезапустить туннель",
                hint: if on { "поднять заново на том же сервере" } else { "туннель выключен" },
                enabled: on,
                onclick: {
                    let controller = controller.clone();
                    move |_| controller.send(Command::Restart)
                },
                icon: rsx! { crate::icons::Refresh { size: 17, color: ACCENT } },
            }
            ActionCard {
                title: "Папка bliss",
                hint: "журнал, движок, конфиг движка",
                enabled: true,
                onclick: {
                    let controller = controller.clone();
                    move |_| controller.send(Command::Reveal(Reveal::WorkDir))
                },
                icon: rsx! { crate::icons::Folder { size: 17, color: ACCENT } },
            }

            if !frame.subscriptions.is_empty() {
                ActionCard {
                    title: if refreshing { "Обновляю подписки…" } else { "Обновить подписки" },
                    hint: plural(frame.subscriptions.len() as u64, "подписка", "подписки", "подписок"),
                    enabled: !refreshing,
                    onclick: {
                        let controller = controller.clone();
                        move |_| controller.send(Command::Refresh(None))
                    },
                    icon: rsx! { crate::icons::Rss { size: 17, color: ACCENT } },
                }
            }

            SystemProxy { frame: frame.clone() }
        }
    }
}

/// The machine's proxy settings, as the three choices every client here offers.
///
/// Laid out as a group of three rather than a switch because the third one is
/// not the opposite of the first: "leave it alone" and "turn it off" are
/// different instructions, and a two-state control cannot say both.
#[component]
fn SystemProxy(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let chosen = frame.proxy_mode;

    // What the machine is actually doing, which is not always what was asked
    // for: "set" waits for the tunnel rather than writing a proxy that points
    // at a port nobody is listening on.
    let state = match (chosen, frame.proxy_applied) {
        (ProxyMode::Set, true) => ("активен", ON.0),
        (ProxyMode::Set, false) => ("применится при подключении", "var(--color-neutral-500)"),
        (ProxyMode::Clear, _) => ("выключен", "var(--color-neutral-400)"),
        (ProxyMode::Unchanged, _) => ("как было", "var(--color-neutral-500)"),
    };

    rsx! {
        div {
            style: "grid-column:1 / -1;display:flex;flex-direction:column;gap:8px;\
                    padding:12px;border-radius:8px;\
                    background-color:rgba(239,232,245,.03)",
            div { style: "display:flex;align-items:center;gap:11px",
                crate::icons::Shield { size: 17, color: ACCENT }
                span { style: "font-size:14.5px", "Системный прокси" }
                span {
                    style: "margin-left:auto;font-size:12.5px;color:{state.1}",
                    "{state.0}"
                }
            }
            // Stacked in a narrow card, side by side in a wide one.
            div { style: "display:grid;grid-template-columns:repeat(auto-fit,minmax(230px,1fr));gap:3px",
                for mode in [ProxyMode::Unchanged, ProxyMode::Set, ProxyMode::Clear] {
                    div {
                        key: "{mode.key()}",
                        class: if mode == chosen { "act choice on" } else { "act choice" },
                        style: "display:flex;align-items:center;gap:9px;padding:6px 9px;border-radius:4px;\
                                font-size:13.5px;\
                                color:{choice_tint(mode == chosen)}",
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Proxy(mode))
                        },
                        // A filled dot rather than a tick: three options of
                        // which exactly one holds, which is what a radio means.
                        div {
                            style: "width:13px;height:13px;flex:none;border-radius:50%;\
                                    box-shadow:inset 0 0 0 1px {choice_ring(mode == chosen)};\
                                    display:grid;place-items:center",
                            if mode == chosen {
                                div {
                                    style: "width:6px;height:6px;border-radius:50%;\
                                            background-color:var(--color-accent)",
                                }
                            }
                        }
                        "{mode.label()}"
                    }
                }
            }
            span {
                style: "font-size:12px;line-height:1.45;color:var(--color-neutral-600)",
                "{proxy_note(chosen)}"
            }
        }
    }
}

fn choice_tint(chosen: bool) -> &'static str {
    if chosen {
        "var(--color-neutral-200)"
    } else {
        "var(--color-neutral-500)"
    }
}

fn choice_ring(chosen: bool) -> &'static str {
    if chosen {
        "var(--color-accent)"
    } else {
        "rgba(239,232,245,.22)"
    }
}

/// One line saying what the choice does to the machine, in plain terms.
///
/// Worth the space: this is the only control in bliss that changes how *other*
/// programs reach the network, and the three words alone do not say that.
///
/// On Linux the setting is the desktop's, GNOME's or KDE's, and what follows
/// it is the programs that read it — browsers and the desktop's own — rather
/// than everything on the machine.
fn proxy_note(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Unchanged if cfg!(windows) => {
            "bliss не трогает настройки Windows. Прокси прописываете сами."
        }
        ProxyMode::Unchanged => "bliss не трогает настройки системы. Прокси прописываете сами.",
        ProxyMode::Set if cfg!(windows) => {
            "Весь трафик Windows идёт через bliss, пока туннель поднят. \
                           При выходе настройки вернутся как были."
        }
        ProxyMode::Set => {
            "Браузеры и программы, что берут прокси из настроек GNOME или KDE, идут через \
             bliss, пока туннель поднят. При выходе настройки вернутся как были."
        }
        ProxyMode::Clear if cfg!(windows) => {
            "Прокси в Windows выключен — на случай, если его оставила \
                             другая программа."
        }
        ProxyMode::Clear => {
            "Прокси в настройках рабочего стола выключен — на случай, если его оставила \
             другая программа."
        }
    }
}

#[component]
fn ActionCard(
    icon: Element,
    title: String,
    hint: String,
    enabled: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let tone = if enabled {
        "color:var(--color-neutral-200)"
    } else {
        "color:var(--color-neutral-700)"
    };
    rsx! {
        div {
            // The resting tint and the hover are `.act-card` in app.css. Inline,
            // the tint outranked the hover, and the cards gave no sign of life.
            class: if enabled { "act act-card" } else { "act act-card disabled" },
            style: "display:flex;align-items:center;gap:11px;padding:12px;border-radius:8px;{tone}",
            onclick: move |e| if enabled { onclick.call(e) },
            {icon}
            div { style: "display:flex;flex-direction:column;gap:2px;min-width:0",
                span { style: "font-size:14.5px", "{title}" }
                span { style: "font-size:12.5px;color:var(--color-neutral-600)", "{hint}" }
            }
        }
    }
}

/// A small button with an icon, for the one or two places a card needs one.
#[component]
fn Button(
    icon: Element,
    label: String,
    enabled: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let tone = if enabled {
        "color:var(--color-neutral-200);box-shadow:inset 0 0 0 1px rgba(238,130,196,.35)"
    } else {
        "color:var(--color-neutral-700);box-shadow:inset 0 0 0 1px rgba(239,232,245,.07)"
    };
    rsx! {
        div {
            class: if enabled { "act" } else { "act disabled" },
            style: "display:inline-flex;align-items:center;gap:7px;padding:6px 13px;border-radius:4px;\
                    font-size:14px;white-space:nowrap;flex:none;{tone}",
            onclick: move |e| if enabled { onclick.call(e) },
            {icon}
            "{label}"
        }
    }
}

/// Is a sweep under way? Both buttons that start one ask this, so they cannot
/// disagree about whether it is available.
fn sweeping(frame: &Frame) -> bool {
    frame.servers.iter().any(|s| s.ping == Ping::Probing)
}

fn ping_text(ping: Ping) -> String {
    use Ping;
    match ping {
        Ping::Unknown => "—".to_owned(),
        Ping::Probing => "…".to_owned(),
        Ping::Ok(ms) => format!("{ms} мс"),
        Ping::Down => "нет".to_owned(),
        Ping::Udp => "UDP".to_owned(),
    }
}

/// Three bands, not a gradient: the eye reads "fine / usable / slow" and a
/// per-millisecond colour would only invite comparing numbers that move.
fn ping_tone(ping: Ping) -> &'static str {
    use Ping;
    match ping {
        Ping::Ok(ms) if ms < 120 => GOOD,
        Ping::Ok(ms) if ms < 300 => "var(--color-accent-2-300)",
        Ping::Ok(_) => "var(--color-neutral-400)",
        Ping::Down => BAD,
        Ping::Unknown | Ping::Probing | Ping::Udp => "var(--color-neutral-600)",
    }
}

#[component]
fn StatRow(label: String, value: String) -> Element {
    rsx! {
        div { style: "display:flex;justify-content:space-between;align-items:baseline;gap:10px",
            span { style: "font-size:13.5px;color:var(--color-neutral-500)", "{label}" }
            span { style: "font-size:15px;text-align:right",
                Ticking { text: value }
            }
        }
    }
}

/// Text whose digits keep still while the number under them changes.
///
/// Each digit goes in a box of the widest digit's width and is centred in it —
/// which is what a tabular figure is, and what this font cannot do by itself
/// (`FIGURE_EM`). Everything else keeps its natural width, so a comma or a unit
/// reads as it was drawn.
#[component]
fn Ticking(text: String) -> Element {
    rsx! {
        for (i, glyph) in text.chars().enumerate() {
            if glyph.is_ascii_digit() {
                span { key: "{i}", class: "figure", "{glyph}" }
            } else {
                span { key: "{i}", "{glyph}" }
            }
        }
    }
}

/// Border, outer glow and inner bloom for a state.
fn tones(state: State) -> (&'static str, &'static str, &'static str) {
    match state {
        State::Running => ON,
        State::Starting => BUSY,
        State::Stopped => IDLE,
        State::Failed => FAILED,
    }
}

/// The connections table's columns: host, program, route, received, sent.
///
/// The first two take the room that is left and never more. A bare `1.6fr`
/// track is at least as wide as its longest unbreakable word, and a host name
/// has no break in it; every row is a grid of its own, so in a narrow window a
/// long name moved its own row's columns and no other's.
const CONNECTION_COLUMNS: &str =
    "grid-template-columns:minmax(0,1.6fr) minmax(0,1fr) 78px 92px 92px;gap:10px";

#[component]
fn Connections(frame: Frame) -> Element {
    // Same click-to-copy as the log below it, and the same note.
    let mut copied = use_signal(String::new);
    let note = copied();
    let query = TABLE_QUERY();
    let rule = TABLE_RULE();
    let rows = table_rows(&frame.connections, &query, rule.as_deref(), TABLE_SORT());
    let choices = rule_choices(&frame.routing, &frame.connections);
    let narrowed = !query.trim().is_empty() || rule.is_some();
    let nothing = match &rule {
        Some(rule) if query.trim().is_empty() => {
            format!("Сейчас нет соединений по правилу «{}».", elide(rule, 32))
        }
        _ => format!("Ничего не нашлось по «{}».", elide(&query, 32)),
    };
    // The table is copied into the frame only while this screen is open to
    // show it (`Command::Table`), so it is asked for on the way in and let go
    // on the way out.
    let controller = use_context::<Controller>();
    use_hook({
        let controller = controller.clone();
        move || {
            TABLE_OPEN.store(true, std::sync::atomic::Ordering::Relaxed);
            controller.send(Command::Table(true));
        }
    });
    // The menus belong to this screen; leaving the screen closes them.
    use_drop(move || {
        TABLE_OPEN.store(false, std::sync::atomic::Ordering::Relaxed);
        controller.send(Command::Table(false));
        *ROW_MENU.write() = None;
        *RULE_MENU.write() = false;
    });

    rsx! {
        div {
            // The same padding as the other screens, so the rail's card lines up
            // with this one's too.
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px 18px 12px",
            div { style: "{SCREEN_HEAD}",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Логи и трафик" }
                // Clipped rather than wrapped: the heading and the buttons beside
                // it keep one line, and this is the part that can give way.
                span { style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;overflow:hidden;white-space:nowrap",
                    "{plural(frame.active, \"соединение\", \"соединения\", \"соединений\")} сейчас · {frame.opened} за сессию"
                }
                span {
                    style: "margin-left:auto;font-size:13.5px;color:var(--color-neutral-500);\
                            font-variant-numeric:tabular-nums;white-space:nowrap;flex:none",
                    "↓ {rate(frame.down_rate)}   ↑ {rate(frame.up_rate)}"
                }
                Button {
                    label: "Скопировать таблицу",
                    enabled: !rows.is_empty(),
                    onclick: {
                        // What is on screen: searched, and in the order shown.
                        let rows: Vec<bliss_core::Snapshot> =
                            rows.iter().map(|row| (*row).clone()).collect();
                        move |_| {
                            let text =
                                rows.iter().map(row_as_text).collect::<Vec<_>>().join("\n");
                            copied.set(if crate::clipboard::copy(&text) {
                                format!("скопировано {}", crate::clipboard::lines(rows.len()))
                            } else {
                                "буфер обмена занят другой программой".to_owned()
                            });
                        }
                    },
                    icon: rsx! { crate::icons::Copy { size: 15, color: ACCENT } },
                }
            }

            div { style: "flex:none;display:flex;align-items:center;gap:12px",
                // The box keeps its width and the hint beside it gives way:
                // squeezed by the hint, it cut its own placeholder short.
                div { class: "search", style: "flex:0 1 380px;min-width:220px",
                    crate::icons::Search { size: 15, color: MUTED }
                    input {
                        class: "field bare",
                        r#type: "text",
                        value: "{query}",
                        placeholder: "Программа или адрес",
                        autocomplete: "off",
                        spellcheck: "false",
                        style: "flex:1;min-width:0;padding:7px 0;border:none;\
                                color:var(--color-text);font-size:14px",
                        oninput: move |e| *TABLE_QUERY.write() = e.value(),
                    }
                    if !query.is_empty() {
                        IconButton {
                            title: "Очистить",
                            enabled: true,
                            onclick: move |_| TABLE_QUERY.write().clear(),
                            icon: rsx! { crate::icons::Close { size: 14, color: MUTED } },
                        }
                    }
                }
                RuleFilter { choices, total: frame.connections.len() }
                span {
                    style: "flex:1 1 0;font-size:12.5px;color:var(--color-neutral-600);min-width:0;\
                            overflow:hidden;white-space:nowrap",
                    if !note.is_empty() {
                        "{note}"
                    } else if narrowed {
                        "показано {rows.len()} из {frame.connections.len()}"
                    } else {
                        "правый клик по строке — правило для её программы или сайта"
                    }
                }
            }

            div {
                // The same gutter as the scrolling rows under it: `.list-head`.
                class: "list-head",
                style: "display:grid;{CONNECTION_COLUMNS};padding:0 8px 7px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                span { "Хост" }
                span { "Процесс" }
                span { "Маршрут" }
                SortHeader { label: "Приём", most: Sort::DownMost, least: Sort::DownLeast }
                SortHeader { label: "Отдача", most: Sort::UpMost, least: Sort::UpLeast }
            }

            div { style: "flex:1;min-height:90px;overflow-y:scroll;overflow-x:hidden",
                // The first frame after the screen opens may still be one
                // without the table: nothing yet, rather than "empty".
                if !frame.table {
                } else if frame.connections.is_empty() {
                    div {
                        style: "padding:28px 8px;font-size:14.5px;color:var(--color-neutral-600)",
                        "Пока пусто. Направьте что-нибудь в socks5://127.0.0.1:{frame.listen.split(' ').nth(1).unwrap_or_default()}"
                    }
                } else if rows.is_empty() {
                    div {
                        style: "padding:28px 8px;font-size:14.5px;color:var(--color-neutral-600)",
                        "{nothing}"
                    }
                }
                for row in rows.iter().copied() {
                    div {
                        key: "{row.id}",
                        class: "row",
                        style: "display:grid;{CONNECTION_COLUMNS};align-items:center;padding:7px 8px;\
                                border-bottom:1px solid rgba(239,232,245,.05)",
                        onclick: {
                            let text = row_as_text(row);
                            move |_| {
                                copied.set(if crate::clipboard::copy(&text) {
                                    format!("скопировано: {}", elide(&text, 44))
                                } else {
                                    "буфер обмена занят другой программой".to_owned()
                                });
                            }
                        },
                        // A rule for its program or its site, right from here.
                        oncontextmenu: {
                            let row = row.clone();
                            move |e: MouseEvent| {
                                e.prevent_default();
                                let point = e.client_coordinates();
                                *ROW_MENU.write() = Some(row_menu(&row, point.x, point.y));
                            }
                        },
                        // One line each, cut at the column's edge: a name that
                        // wrapped at its hyphens made its row twice as tall.
                        span {
                            style: "min-width:0;overflow:hidden;white-space:nowrap;font-size:14px;\
                                    font-family:ui-monospace,Consolas,monospace",
                            title: "{row.target}",
                            "{elide(&row.target, 40)}"
                        }
                        span {
                            style: "min-width:0;overflow:hidden;white-space:nowrap;font-size:13.5px;\
                                    color:var(--color-neutral-400)",
                            "{row.process.clone().unwrap_or_else(|| String::from(\"—\"))}"
                        }
                        span {
                            style: "overflow:hidden;white-space:nowrap;font-size:12.5px;\
                                    color:{route_color(row.closed_at.is_some())}",
                            title: "правило: {row.rule}",
                            if row.closed_at.is_some() { "закрыто" } else { "{route_short(row.route)}" }
                        }
                        span {
                            style: "text-align:right;white-space:nowrap;font-size:13.5px;\
                                    font-variant-numeric:tabular-nums;color:var(--color-neutral-300)",
                            "{bytes(row.down)}"
                        }
                        span {
                            style: "text-align:right;white-space:nowrap;font-size:13.5px;\
                                    font-variant-numeric:tabular-nums;color:var(--color-neutral-300)",
                            "{bytes(row.up)}"
                        }
                    }
                }
            }

            // Inside the padded column, not after it: a closing brace once
            // ended the column early, and the log ran edge to edge beneath it.
            LogDock { revision: crate::logs::revision() }
        }
        if let Some(menu) = ROW_MENU() {
            RowMenuView { menu, copied, vpn: frame.vpn.name.is_some() }
        }
    }
}

// --- screen: routing ------------------------------------------------------

/// The rule table, a box for asking it questions, and the editor.
///
/// A rule is added with the button over the list or from a connection's
/// right-click menu, and changed by clicking it. Changes decide the next
/// connection, with the tunnel left up.
#[component]
fn Routing(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let view = &frame.routing;
    let query = use_signal(String::new);
    let suggested = suggested_action(view.fallback);
    // The editor belongs to this screen; leaving the screen closes it.
    use_drop(|| *DRAFT.write() = None);

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px 18px 12px",

            div { style: "{SCREEN_HEAD}",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Маршрутизация" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;overflow:hidden;white-space:nowrap",
                    "{rule_count(view)} · остальное {view.fallback.label()}"
                }
                div { style: "margin-left:auto;flex:none;align-self:center",
                    Button {
                        label: "Добавить правило",
                        enabled: true,
                        onclick: move |_| new_rule(suggested, ""),
                        icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                    }
                }
            }

            if !view.rules.is_empty() || !view.presets.is_empty() {
                PresetBar { view: view.clone() }
            }

            // No rules at all: offer the starting set rather than an empty list.
            if view.rules.is_empty() {
                div {
                    style: "flex:none;display:flex;align-items:center;gap:13px;padding:16px;\
                            border-radius:8px;background-color:var(--color-surface);\
                            box-shadow:inset 0 0 0 1px rgba(238,130,196,.24)",
                    div { style: "display:flex;flex-direction:column;gap:3px;min-width:0",
                        span { style: "font-size:14.5px", "Правил пока нет" }
                        span {
                            style: "font-size:13px;line-height:1.45;color:var(--color-neutral-500)",
                            "Сейчас всё идёт через прокси. Можно добавить стартовый набор — \
                             реклама блокируется, локальная сеть идёт напрямую, — или своё \
                             правило кнопкой вверху."
                        }
                    }
                    div { style: "margin-left:auto;flex:none",
                        Button {
                            label: "Добавить набор",
                            enabled: true,
                            onclick: {
                                let controller = controller.clone();
                                move |_| controller.send(Command::SeedRules)
                            },
                            icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                        }
                    }
                }
            }

            if !view.warnings.is_empty() {
                div {
                    style: "flex:none;display:flex;flex-direction:column;gap:5px;padding:12px;\
                            border-radius:8px;\
                            background-color:rgba(203,186,244,.07);\
                            box-shadow:inset 0 0 0 1px rgba(203,186,244,.18)",
                    span {
                        style: "font-size:12px;letter-spacing:.09em;text-transform:uppercase;\
                                color:var(--color-neutral-500)",
                        "Что в правилах не сработает"
                    }
                    for (n, warning) in view.warnings.iter().enumerate() {
                        span {
                            key: "{n}",
                            style: "font-size:13px;line-height:1.45;color:#cbbaf4;\
                                    word-break:break-word",
                            "{warning}"
                        }
                    }
                }
            }

            div { style: "flex:1;min-height:0;display:flex;gap:13px;align-items:stretch",
                RuleList { view: view.clone(), vpn: frame.vpn.name.is_some() }
                Checker {
                    query,
                    result: frame.check.clone(),
                    suggested,
                    onask: {
                        let controller = controller.clone();
                        move |text: String| controller.send(Command::Check(text))
                    },
                }
            }
        }
        if let Some(draft) = DRAFT() {
            RuleEditor { draft, vpn: frame.vpn.name.is_some() }
        }
    }
}

/// One preset's pill, on the routing screen and the main one. A click puts
/// every switch where the preset has it — for the one in force whose switches
/// have moved since, back where they were.
#[component]
fn PresetChip(name: String, current: bool, drifted: bool) -> Element {
    let controller = use_context::<Controller>();
    let class = match (current, drifted) {
        (false, _) => "preset-chip",
        (true, false) => "preset-chip on",
        (true, true) => "preset-chip on drifted",
    };
    let title = match (current, drifted) {
        (false, _) => format!("Включить пресет «{name}»: правила встанут, как в нём"),
        (true, false) => format!("Пресет «{name}» в силе"),
        (true, true) => format!("Правила сдвинулись с «{name}» — нажмите, чтобы вернуть как было"),
    };
    let label = name.clone();
    rsx! {
        span {
            class,
            title,
            onclick: move |_| controller.send(Command::ApplyPreset(name.clone())),
            "{label}"
        }
    }
}

/// What the name box is for: a new preset, or the one in force renamed.
#[derive(Clone, Debug, PartialEq)]
enum Naming {
    New,
    Rename(String),
}

/// The presets on the routing screen: a pill each, the tools of the one in
/// force beside them, and a box to name a new one.
///
/// A preset is where every switch stood, and where everything else went, when
/// it was saved. The one in force wears a violet dot once a switch has moved
/// since; the tick takes the new positions into it.
#[component]
fn PresetBar(view: crate::engine::RoutingView) -> Element {
    let controller = use_context::<Controller>();
    let mut naming = use_signal(|| None::<Naming>);
    let mut text = use_signal(String::new);
    // The preset whose delete was clicked once; the second click deletes it.
    let mut armed = use_signal(|| None::<String>);

    let mut submit = {
        let controller = controller.clone();
        move || {
            let name = text();
            match naming() {
                Some(Naming::New) => controller.send(Command::SavePreset(name)),
                Some(Naming::Rename(from)) => {
                    controller.send(Command::RenamePreset { from, to: name })
                }
                None => {}
            }
            naming.set(None);
        }
    };
    // Cloned before rsx!, which builds the handlers in an order of its own
    // (rule 13 in AGENTS.md).
    let mut submit_on_key = submit.clone();
    let (save, delete) = (controller.clone(), controller.clone());
    let current = view.current_preset.clone().filter(|_| naming().is_none());
    let pills: Vec<(String, bool)> = view
        .presets
        .iter()
        .map(|name| {
            (
                name.clone(),
                view.current_preset.as_deref() == Some(name.as_str()),
            )
        })
        .collect();

    rsx! {
        div {
            style: "flex:none;display:flex;align-items:center;flex-wrap:wrap;gap:6px",
            span {
                style: "margin-right:4px;font-size:12px;letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500)",
                "Пресеты"
            }
            for (name, current) in pills {
                PresetChip { key: "{name}", name: name.clone(), current, drifted: view.drifted }
            }
            if view.presets.is_empty() && naming().is_none() {
                span { style: "font-size:13px;color:var(--color-neutral-600)",
                    "положения переключателей под именем — переключаются одним кликом"
                }
            }
            if let Some(name) = current {
                div { style: "display:flex;align-items:center;gap:2px",
                    IconButton {
                        title: if view.drifted {
                            format!("Сохранить новые положения в «{name}»")
                        } else {
                            format!("«{name}» сохранён как есть")
                        },
                        enabled: view.drifted,
                        onclick: {
                            let name = name.clone();
                            move |_| save.send(Command::SavePreset(name.clone()))
                        },
                        icon: rsx! {
                            crate::icons::Check { size: 16, color: if view.drifted { ACCENT } else { MUTED } }
                        },
                    }
                    IconButton {
                        title: format!("Переименовать «{name}»"),
                        enabled: true,
                        onclick: {
                            let name = name.clone();
                            move |_| {
                                text.set(name.clone());
                                naming.set(Some(Naming::Rename(name.clone())));
                            }
                        },
                        icon: rsx! { crate::icons::Pencil { size: 15, color: MUTED } },
                    }
                    IconButton {
                        title: if armed().as_deref() == Some(name.as_str()) {
                            format!("Нажмите ещё раз, чтобы удалить «{name}»")
                        } else {
                            format!("Удалить пресет «{name}» — правила останутся как есть")
                        },
                        enabled: true,
                        danger: true,
                        onclick: {
                            let name = name.clone();
                            move |_| {
                                if armed().as_deref() == Some(name.as_str()) {
                                    armed.set(None);
                                    delete.send(Command::DeletePreset(name.clone()));
                                } else {
                                    armed.set(Some(name.clone()));
                                }
                            }
                        },
                        icon: rsx! {
                            crate::icons::Trash {
                                size: 15,
                                color: if armed().as_deref() == Some(name.as_str()) { BAD } else { MUTED },
                            }
                        },
                    }
                }
            }
            if naming().is_none() {
                div { style: "margin-left:auto",
                    Button {
                        label: "Сохранить как пресет",
                        enabled: true,
                        onclick: move |_| {
                            text.set(String::new());
                            naming.set(Some(Naming::New));
                        },
                        icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                    }
                }
            } else {
                div { style: "display:flex;align-items:center;gap:4px",
                    input {
                        class: "field",
                        r#type: "text",
                        value: "{text}",
                        maxlength: "{crate::profile_file::MAX_PRESET_NAME}",
                        placeholder: if matches!(naming(), Some(Naming::New)) {
                            "Название, например «Игры»"
                        } else {
                            "Новое название"
                        },
                        autocomplete: "off",
                        spellcheck: "false",
                        style: "width:200px;padding:6px 9px;border-radius:4px;border:none;\
                                color:var(--color-text);font-size:13.5px",
                        onmounted: move |e: MountedEvent| async move {
                            let _ = e.data().set_focus(true).await;
                        },
                        oninput: move |e| text.set(e.value()),
                        onkeydown: move |e: KeyboardEvent| match e.key() {
                            Key::Enter => {
                                e.prevent_default();
                                if !text().trim().is_empty() {
                                    submit_on_key();
                                }
                            }
                            Key::Escape => naming.set(None),
                            _ => {}
                        },
                    }
                    IconButton {
                        title: "Сохранить",
                        enabled: !text().trim().is_empty(),
                        onclick: move |_| submit(),
                        icon: rsx! { crate::icons::Check { size: 16, color: ACCENT } },
                    }
                    IconButton {
                        title: "Отмена",
                        enabled: true,
                        onclick: move |_| naming.set(None),
                        icon: rsx! { crate::icons::Close { size: 14, color: MUTED } },
                    }
                }
            }
        }
    }
}

/// The presets on the main screen, beside its heading: one line, and those
/// that do not fit are hidden rather than wrapped into a second (rule 19).
#[component]
fn PresetPills(view: crate::engine::RoutingView) -> Element {
    let pills: Vec<(String, bool)> = view
        .presets
        .iter()
        .map(|name| {
            (
                name.clone(),
                view.current_preset.as_deref() == Some(name.as_str()),
            )
        })
        .collect();
    rsx! {
        div {
            style: "margin-left:auto;align-self:center;flex:0 1 auto;min-width:0;height:26px;\
                    overflow:hidden;display:flex;flex-wrap:wrap;justify-content:flex-end;\
                    align-items:center;gap:6px",
            span { style: "font-size:12.5px;color:var(--color-neutral-600);white-space:nowrap",
                "пресет"
            }
            for (name, current) in pills {
                PresetChip { key: "{name}", name: name.clone(), current, drifted: view.drifted }
            }
        }
    }
}

/// `5 правил`, or `5 правил, 2 выключены` when some are off.
fn rule_count(view: &crate::engine::RoutingView) -> String {
    let total = plural(view.rules.len() as u64, "правило", "правила", "правил");
    match view.rules.iter().filter(|rule| !rule.enabled).count() {
        0 => total,
        off => format!(
            "{total}, {}",
            plural(off as u64, "выключено", "выключены", "выключено")
        ),
    }
}

/// A rule's switch: the TUN switch's look, small, and pink while the rule
/// decides. The click stops here — the row under it opens the editor.
#[component]
fn RuleSwitch(at: usize, on: bool) -> Element {
    let controller = use_context::<Controller>();
    rsx! {
        div {
            class: if on { "rule-switch on" } else { "rule-switch" },
            title: if on { "Правило работает — выключить" } else { "Правило выключено — включить" },
            onclick: move |e: MouseEvent| {
                e.stop_propagation();
                controller.send(Command::SwitchRule { at, on: !on });
            },
            div { class: "rule-knob" }
        }
    }
}

#[component]
fn RuleList(view: crate::engine::RoutingView, vpn: bool) -> Element {
    let controller = use_context::<Controller>();
    // The rule whose delete was clicked once; the second click deletes it.
    let mut armed = use_signal(|| None::<usize>);
    let count = view.rules.len();

    rsx! {
        div {
            style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:9px;padding:16px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            div { style: "display:flex;align-items:baseline;gap:10px",
                span { style: "font:700 16px/1 var(--font-heading)", "Правила" }
                span {
                    style: "font-size:13px;color:var(--color-neutral-600);min-width:0;\
                            overflow:hidden;white-space:nowrap",
                    "сверху вниз, срабатывает первое · клик — изменить"
                }
            }

            div {
                style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;\
                        display:flex;flex-direction:column;gap:5px",
                if view.rules.is_empty() {
                    span {
                        style: "padding:14px 2px;font-size:14px;line-height:1.5;\
                                color:var(--color-neutral-600)",
                        "Ни одного правила — всё решает строка «Всё остальное». Своё правило \
                         добавляется кнопкой вверху или правым кликом по соединению в «Логах \
                         и трафике»."
                    }
                }
                for (at, rule) in view.rules.iter().enumerate() {
                    div {
                        key: "{at}",
                        // Off by class, not by inline style (rule 1): the text and
                        // the action fade, the switch does not.
                        class: if rule.enabled { "rule-row" } else { "rule-row off" },
                        // Wraps: the action and the tools go under the text
                        // when they do not fit beside the number and 240px of
                        // text. Beside it at any width, the text column
                        // narrowed to nothing in a narrow window, and its words
                        // broke a letter to a line.
                        style: "display:flex;flex-wrap:wrap;align-items:flex-start;gap:6px 11px;\
                                padding:9px 10px;border-radius:4px",
                        onclick: {
                            let source = view.source.get(at).cloned();
                            move |_| {
                                if let Some(rule) = &source {
                                    edit_rule(at, rule);
                                }
                            }
                        },
                        onmouseleave: move |_| {
                            if armed() == Some(at) {
                                armed.set(None);
                            }
                        },
                        // The number and the text wrap as one: as two items,
                        // the number took a line of its own in a narrow window.
                        div {
                            class: "rule-body",
                            style: "flex:1 1 270px;min-width:0;display:flex;align-items:flex-start;gap:11px",
                            // The number is the rule: first match wins, so the
                            // order is not decoration.
                            span {
                                style: "flex:none;width:18px;padding-top:1px;font-size:13px;\
                                        text-align:right;color:var(--color-neutral-700);\
                                        font-variant-numeric:tabular-nums",
                                "{rule.position}"
                            }
                            div { style: "flex:1;display:flex;flex-direction:column;gap:3px;min-width:0",
                                span { style: "font-size:14px", "{elide(&rule.name, 46)}" }
                                for (n, line) in rule.conditions.iter().enumerate() {
                                    span {
                                        key: "{n}",
                                        style: "font-size:12.5px;line-height:1.4;\
                                                color:var(--color-neutral-600);word-break:break-word",
                                        "{elide(line, 64)}"
                                    }
                                }
                            }
                        }
                        div { style: "flex:none;margin-left:auto;display:flex;align-items:center;gap:8px",
                            RuleSwitch { at, on: rule.enabled }
                            div { class: "rule-action", ActionPill { action: rule.action } }
                            div { class: "rule-tools",
                                IconButton {
                                    title: "Выше: решает раньше",
                                    enabled: at > 0,
                                    onclick: {
                                        let controller = controller.clone();
                                        move |_| {
                                            controller.send(Command::MoveRule {
                                                from: at,
                                                to: at.saturating_sub(1),
                                            })
                                        }
                                    },
                                    icon: rsx! { crate::icons::ChevronUp { size: 16, color: MUTED } },
                                }
                                IconButton {
                                    title: "Ниже",
                                    enabled: at + 1 < count,
                                    onclick: {
                                        let controller = controller.clone();
                                        move |_| controller.send(Command::MoveRule { from: at, to: at + 1 })
                                    },
                                    icon: rsx! { crate::icons::ChevronDown { size: 16, color: MUTED } },
                                }
                                IconButton {
                                    title: "Изменить",
                                    enabled: true,
                                    onclick: {
                                        let source = view.source.get(at).cloned();
                                        move |_| {
                                            if let Some(rule) = &source {
                                                edit_rule(at, rule);
                                            }
                                        }
                                    },
                                    icon: rsx! { crate::icons::Pencil { size: 15, color: MUTED } },
                                }
                                IconButton {
                                    title: if armed() == Some(at) { "Нажмите ещё раз, чтобы удалить" } else { "Удалить" },
                                    enabled: true,
                                    danger: true,
                                    onclick: {
                                        let controller = controller.clone();
                                        move |_| {
                                            if armed() == Some(at) {
                                                armed.set(None);
                                                controller.send(Command::DeleteRule(at));
                                            } else {
                                                armed.set(Some(at));
                                            }
                                        }
                                    },
                                    icon: rsx! {
                                        crate::icons::Trash {
                                            size: 15,
                                            color: if armed() == Some(at) { BAD } else { MUTED },
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }

            div {
                // Wraps: in the default window the label and three choices do
                // not share one line, and squeezed, the label broke in two.
                style: "display:flex;align-items:center;flex-wrap:wrap;gap:6px;padding-top:9px;\
                        border-top-width:1px;border-top-style:solid;border-top-color:var(--color-divider)",
                span {
                    style: "margin-right:auto;font-size:13.5px;white-space:nowrap;\
                            color:var(--color-neutral-500)",
                    "Всё остальное"
                }
                for action in actions(vpn, view.fallback) {
                    span {
                        key: "{action_key(action)}",
                        class: choice_class("pill-choice", action == view.fallback, action),
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Fallback(action))
                        },
                        "{action.label()}"
                    }
                }
            }
        }
    }
}

/// "What happens to this?" — the same question `bliss route` answers, in the
/// window, and without connecting to anything.
#[component]
fn Checker(
    query: Signal<String>,
    result: Option<crate::engine::CheckResult>,
    /// The action a rule started from here begins with.
    suggested: Action,
    onask: EventHandler<String>,
) -> Element {
    // `geoip:ru` is a condition for a rule, not an address to check — and it
    // is exactly what someone looking for a rule editor typed into this box.
    let condition = crate::rule_text::looks_like_condition(&query());
    rsx! {
        div {
            style: "width:288px;flex:none;display:flex;flex-direction:column;gap:11px;\
                    padding:16px;background-color:var(--color-surface);\
                    border-radius:8px;box-shadow:var(--shadow-sm)",
            span { style: "font:700 16px/1 var(--font-heading)", "Проверить адрес" }
            span {
                style: "font-size:12.5px;line-height:1.45;color:var(--color-neutral-600)",
                "Имя сайта, или адрес, или host:port. Никуда не подключается — \
                 только прогоняет по правилам."
            }

            input {
                class: "field",
                r#type: "text",
                value: "{query}",
                placeholder: "youtube.com",
                autocomplete: "off",
                spellcheck: "false",
                style: "padding:8px 10px;border-radius:4px;border:none;\
                        color:var(--color-text);font-size:14px;\
                        font-family:ui-monospace,Consolas,monospace",
                // The text lives here rather than in the frame: a controlled
                // input fed from a value that is republished twice a second
                // fights whoever is typing into it.
                oninput: move |e| {
                    let text = e.value();
                    query.set(text.clone());
                    onask.call(text);
                },
                onkeydown: move |e: KeyboardEvent| {
                    if e.key() == Key::Enter && condition {
                        new_rule(suggested, &query());
                    }
                },
            }

            if condition {
                div {
                    style: "display:flex;flex-direction:column;align-items:flex-start;gap:9px;\
                            padding:12px;border-radius:4px;background-color:rgba(238,130,196,.07);\
                            box-shadow:inset 0 0 0 1px rgba(238,130,196,.22)",
                    span { style: "font-size:13px;line-height:1.45;color:var(--color-neutral-300)",
                        "Это условие для правила, а здесь проверяется адрес сайта. \
                         Правило с этим условием можно создать сразу."
                    }
                    Button {
                        label: "Создать правило",
                        enabled: true,
                        onclick: move |_| new_rule(suggested, &query()),
                        icon: rsx! { crate::icons::Plus { size: 15, color: ACCENT } },
                    }
                }
            } else if let Some(answer) = result {
                div {
                    style: "display:flex;flex-direction:column;gap:7px;padding:11px;\
                            border-radius:4px;background-color:rgba(239,232,245,.03)",
                    span {
                        style: "font-size:13px;color:var(--color-neutral-400);\
                                font-family:ui-monospace,Consolas,monospace;word-break:break-all",
                        "{answer.target}"
                    }
                    div { style: "display:flex;align-items:center;gap:9px",
                        ActionPill { action: answer.action }
                        span {
                            style: "font-size:12.5px;color:var(--color-neutral-500);min-width:0",
                            "{elide(&answer.rule, 24)}"
                        }
                    }
                    // A site that walked past a rule with a country: why it did.
                    if let Some(note) = &answer.note {
                        span {
                            style: "font-size:12.5px;line-height:1.45;color:#cbbaf4",
                            "{note}"
                        }
                    }
                }
            } else {
                span { style: "font-size:13px;color:var(--color-neutral-700)", "—" }
            }
        }
    }
}

/// The action, as a coloured word. Three actions, three colours, used
/// everywhere the answer appears so the colour alone carries it.
#[component]
fn ActionPill(action: bliss_proto::Action) -> Element {
    let (text, border) = match action {
        bliss_proto::Action::Proxy => ("rgba(238,130,196,.14)", ACCENT),
        bliss_proto::Action::Direct => ("rgba(114,207,158,.14)", GOOD),
        bliss_proto::Action::Block => ("rgba(229,107,111,.14)", BAD),
        bliss_proto::Action::Vpn => ("rgba(126,179,238,.14)", VPN_TONE),
    };
    rsx! {
        // One width for all three words: the pills stand in a column down the
        // rule list, and every rule's tools wrap under it at the same width.
        span {
            style: "display:inline-block;min-width:112px;text-align:center;padding:3px 9px;\
                    border-radius:11px;font-size:12.5px;white-space:nowrap;\
                    background-color:{text};color:{border}",
            "{action.label()}"
        }
    }
}

fn route_color(closed: bool) -> &'static str {
    if closed {
        "var(--color-neutral-600)"
    } else {
        "var(--color-accent-300)"
    }
}

/// Trim to a width. `text-overflow` is not in the renderer subset we committed
/// to, so the ellipsis is ours.
fn elide(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_owned();
    }
    let keep: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}\u{2026}")
}

/// Russian agreement: 1 соединение, 2 соединения, 5 соединений.
///
/// Machine-written text gives itself away here more than anywhere else, and the
/// rule is four lines.
pub(crate) fn plural(n: u64, one: &str, few: &str, many: &str) -> String {
    let form = match (n % 10, n % 100) {
        (1, rest) if rest != 11 => one,
        (2..=4, rest) if !(12..=14).contains(&rest) => few,
        _ => many,
    };
    format!("{n} {form}")
}

/// Horizontal distance between two samples, in the chart's own units.
const CHART_STEP: f64 = 10.0;
/// The chart's height in its own units. The top of the range stands at
/// `CHART_TOP`, short of the edge, so a line along it is not cut off.
const CHART_HEIGHT: f64 = 100.0;
const CHART_TOP: f64 = 90.0;
/// The line while nothing flows: a quiet baseline, because pink says traffic.
const CHART_IDLE: &str = "rgba(239,232,245,.16)";

/// The classes of the chart's gliding layer.
///
/// A CSS animation starts again only when its name changes, so consecutive
/// samples take two identical glides in turn, by the parity of the count. A
/// chart with nothing to draw, or one opened since the last sample, stands
/// where a glide ends, and the next sample moves it on from exactly there.
/// Should the window ever miss a whole tick, the parity comes back unchanged
/// and that one glide is skipped.
fn glide_class(samples: u64, opened: u64, idle: bool) -> &'static str {
    if idle || samples == opened {
        "glide still"
    } else if samples.is_multiple_of(2) {
        "glide a"
    } else {
        "glide b"
    }
}

/// How far the group around the chart's paths stretches one KB/s, so that the
/// range's top stands at `CHART_TOP`.
fn chart_lift(scale: u64) -> f64 {
    CHART_TOP / (scale.max(1) as f64 / 1024.0)
}

/// The chart's line through its samples and the area under it: `CHART_STEP`
/// apart, one KB/s to a unit, the baseline at zero.
fn chart_paths(history: &[u64]) -> (String, String) {
    let points: Vec<(f64, f64)> = history
        .iter()
        .enumerate()
        .map(|(i, &rate)| (i as f64 * CHART_STEP, rate as f64 / 1024.0))
        .collect();
    let Some(&(first_x, first_y)) = points.first() else {
        return (String::new(), String::new());
    };
    let mut line = format!("M{first_x:.2},{first_y:.2}");
    for [(ax, ay), (bx, by), (x, y)] in monotone_segments(&points) {
        line.push_str(&format!("C{ax:.2},{ay:.2} {bx:.2},{by:.2} {x:.2},{y:.2}"));
    }
    let last_x = points.last().map_or(first_x, |point| point.0);
    let area = format!("{line}L{last_x:.2},0L{first_x:.2},0Z");
    (line, area)
}

/// The Bézier segments of a smooth curve through `points`, whose x rises, that
/// never overshoots them: each segment stays between the two samples it joins.
///
/// A spline that merely looks smooth dips below zero beside a burst and draws
/// traffic that never flowed. The slopes are Fritsch and Carlson's, as in d3's
/// monotone curve: flat at a peak or a trough, and never steeper than twice the
/// gentler of the two secants beside a sample.
fn monotone_segments(points: &[(f64, f64)]) -> Vec<[(f64, f64); 3]> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }
    let secant = |i: usize| (points[i + 1].1 - points[i].1) / (points[i + 1].0 - points[i].0);
    let slope = |i: usize| {
        if i == 0 {
            return secant(0);
        }
        if i == n - 1 {
            return secant(n - 2);
        }
        let (before, after) = (secant(i - 1), secant(i));
        if before * after <= 0.0 {
            return 0.0;
        }
        let (h0, h1) = (points[i].0 - points[i - 1].0, points[i + 1].0 - points[i].0);
        let weighted = (before * h1 + after * h0) / (h0 + h1);
        2.0 * before.signum() * before.abs().min(after.abs()).min(weighted.abs() / 2.0)
    };
    (0..n - 1)
        .map(|i| {
            let ((x0, y0), (x1, y1)) = (points[i], points[i + 1]);
            let third = (x1 - x0) / 3.0;
            [
                (x0 + third, y0 + slope(i) * third),
                (x1 - third, y1 - slope(i + 1) * third),
                (x1, y1),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frames Windows chooses between, and the ones `icon_side` asks for.
    /// Most converters write one 256 px frame, and then every window icon is
    /// that frame shrunk, wordmark and all.
    #[test]
    fn the_icon_has_a_frame_for_every_window_icon_size() {
        const ICO: &[u8] = include_bytes!("../assets/bliss.ico");
        assert_eq!(ICO[..4], [0, 0, 1, 0], "not an icon file");
        let count = usize::from(u16::from_le_bytes([ICO[4], ICO[5]]));
        // Each directory entry is 16 bytes and starts with the width; 0 is 256.
        let sides: Vec<u32> = (0..count)
            .map(|i| match ICO[6 + 16 * i] {
                0 => 256,
                side => u32::from(side),
            })
            .collect();
        let mut expected = MARK_FRAMES.to_vec();
        expected.extend([128, 256]);
        assert_eq!(sides, expected);

        // Every scale Windows offers up to 300% lands exactly on a frame, so the
        // shell never resizes one. Any other scale — a custom 275%, or 400% —
        // rounds down to a frame without the wordmark, never up to one with it.
        const OFFERED: [u32; 8] = [100, 125, 150, 175, 200, 225, 250, 300];
        for percent in (100..=500).step_by(25) {
            let scale = f64::from(percent) / 100.0;
            for at_100 in [16.0, 32.0] {
                let side = icon_side(at_100, scale);
                assert!(
                    MARK_FRAMES.contains(&side),
                    "{at_100} px at {percent}%: {side}"
                );
                if OFFERED.contains(&percent) {
                    assert_eq!(
                        f64::from(side),
                        (at_100 * scale).round(),
                        "{at_100} px at {percent}%"
                    );
                }
            }
        }
    }

    /// WebView2 cannot be created in a minimized window, and dioxus-desktop
    /// unwraps the error: minimized as it was built, in `with_on_window`, the
    /// window died at every `--minimized` start before it appeared. `App`
    /// minimizes it once the page exists.
    #[test]
    fn the_window_is_not_minimized_before_its_page_exists() {
        let source = include_str!("ui.rs");
        let start = source.find("pub fn launch(").expect("launch is in ui.rs");
        let end = start + source[start..].find("\n}\n").expect("launch ends");
        let code: String = source[start..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        assert!(
            !code.contains("set_minimized"),
            "launch minimizes the window before WebView2 is created in it"
        );
    }

    /// Hidden with its fade, the menu flashed at the program's place — the
    /// fade follows the window there. The fade goes off before the window is
    /// hidden and comes back only once it stands where the program does.
    #[test]
    fn the_menu_leaves_without_a_fade() {
        let source = include_str!("ui.rs");
        let start = source
            .find("fn out_of_menu(")
            .expect("out_of_menu is in ui.rs");
        let end = start + source[start..].find("\n}\n").expect("it ends");
        let code: String = source[start..end]
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let at = |text: &str| {
            code.find(text)
                .unwrap_or_else(|| panic!("{text} in out_of_menu"))
        };
        assert!(at("animated(window, false)") < at("set_visible(false)"));
        assert!(at("set_outer_position") < at("animated(window, true)"));
    }

    /// A frame nobody can see is not drawn, and whatever shows the window
    /// again from the tray brings the page up to date first — shown before
    /// that, it would stand there with the numbers of whenever it was hidden.
    #[test]
    fn frames_are_drawn_only_where_they_are_seen() {
        let source = include_str!("ui.rs");
        let body = |start: &str| {
            let from = source
                .find(start)
                .unwrap_or_else(|| panic!("{start} in ui.rs"));
            let rest = &source[from..];
            &rest[..rest.find("\n}\n").expect("it ends")]
        };
        let app = body("fn App() -> Element {");
        let asks = app
            .find("if seen(&window)")
            .expect("the loop asks who can see");
        let draws = app
            .find("frame.set(controller.frame())")
            .expect("the loop draws");
        assert!(
            asks < draws,
            "drawn before asking whether anyone can see it"
        );

        let tray = body("fn Tray(frame");
        for show in ["out_of_the_tray(&clicked)", "into_menu(&clicked"] {
            for (at, _) in tray.match_indices(show) {
                let before = &tray[..at];
                let caught = before
                    .rfind("catch_up(")
                    .unwrap_or_else(|| panic!("{show} with nothing caught up"));
                let between = &before[caught..];
                assert!(
                    !between.contains("} else") && !between.contains("=>"),
                    "{show} in a branch that did not catch up"
                );
            }
        }
    }

    /// A window brought out of the tray is shown cloaked and uncloaked once
    /// the page has painted: shown at once, the tray's menu stood for its
    /// first 10 to 70ms as the program cut to its shape.
    #[test]
    fn a_window_is_seen_only_once_it_is_painted() {
        let source = include_str!("ui.rs");
        let body = |start: &str| {
            let from = source
                .find(start)
                .unwrap_or_else(|| panic!("{start} in ui.rs"));
            let rest = &source[from..];
            &rest[..rest.find("\n}\n").expect("it ends")]
        };
        let tray = body("fn Tray(frame");
        for who in ["showing", "bringing"] {
            let cloak = tray
                .find(&format!("cloaked(&{who}, true)"))
                .unwrap_or_else(|| panic!("{who} is cloaked"));
            let show = tray
                .find(&format!("{who}.set_visible(true)"))
                .unwrap_or_else(|| panic!("{who} is shown"));
            let painted = tray[show..]
                .find("when_painted(")
                .unwrap_or_else(|| panic!("{who} waits for the paint"));
            let uncloak = tray[show + painted..]
                .find("cloaked(&")
                .unwrap_or_else(|| panic!("{who} is uncloaked"));
            assert!(cloak < show, "{who} shown before it is cloaked");
            assert!(
                tray[show + painted + uncloak..].starts_with("cloaked(&window, false)")
                    || tray[show + painted + uncloak..].starts_with("cloaked(&showing, false)"),
                "{who} uncloaked by something else"
            );
        }
        // And nothing else shows a hidden window straight away.
        let out = body("fn out_of_the_tray(");
        assert!(
            !out.contains("set_visible(true)"),
            "out_of_the_tray shows it at once"
        );
        assert!(
            PAINTED.contains("setTimeout"),
            "a page that does not paint must not keep it away"
        );
    }

    /// The VPN's choice is offered once there is a VPN — and wherever it is
    /// the choice already, so a rule that sends there says so after the VPN
    /// has been removed.
    #[test]
    fn the_vpn_is_offered_when_there_is_one() {
        assert!(!actions(false, Action::Proxy).contains(&Action::Vpn));
        assert!(actions(true, Action::Proxy).contains(&Action::Vpn));
        assert!(actions(false, Action::Vpn).contains(&Action::Vpn));
        // The CSS has a colour for every choice there can be.
        for action in actions(true, Action::Proxy) {
            let key = action_key(action);
            for class in [".menu-dot.", ".seg-choice.on.", ".pill-choice.on."] {
                assert!(APP_CSS.contains(&format!("{class}{key}")), "{class}{key}");
            }
        }
    }

    /// The menu grows by its presets rather than taking their room from the
    /// server list, four lines at most, and the list's height in app.css is
    /// those four lines.
    #[test]
    fn the_menu_grows_with_its_presets() {
        assert_eq!(menu_height(0), MENU_SIZE.1);
        assert!(menu_height(1) > MENU_SIZE.1);
        assert!(menu_height(3) > menu_height(1));
        assert_eq!(
            menu_height(PRESET_LINES),
            menu_height(24),
            "past four it scrolls"
        );
        let block = APP_CSS
            .split(".tray-presets {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect(".tray-presets is in app.css");
        let tall = format!("max-height: {}px", PRESET_LINE as usize * PRESET_LINES - 2);
        assert!(block.contains(&tall), "{tall} in {block}");
    }

    /// The tray's menu is the window in a small shape, and tao lets a
    /// borderless window be dragged by its edges while it is resizable, and
    /// Win+Up maximizes it while it is maximizable: the menu stretched. The
    /// menu takes both away, and has to give both back, or the program could
    /// never be resized again after the first menu.
    #[test]
    fn the_menu_keeps_its_shape() {
        fn body(name: &str) -> String {
            let source = include_str!("ui.rs");
            let start = source
                .find(&format!("fn {name}("))
                .expect("the function is in ui.rs");
            let end = start + source[start..].find("\n}\n").expect("it ends");
            source[start..end]
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect()
        }
        let (into, out) = (body("into_menu"), body("out_of_menu"));
        for call in ["set_resizable", "set_maximizable"] {
            assert!(
                into.contains(&format!("{call}(false)")),
                "the menu keeps {call}"
            );
            assert!(
                out.contains(&format!("{call}(true)")),
                "the program loses {call}"
            );
        }
    }

    /// WebView2 ignores every `::-webkit-scrollbar` rule on an element that
    /// sets either standard scrollbar property, and draws its own bar, whose
    /// arrows stay when there is nothing to scroll. Set on `*`, they put two
    /// arrows and no bar beside every pane that fit, the settings card first.
    /// So neither is set anywhere: not in the stylesheets, not inline. The
    /// names are assembled here, or this test would find them in itself.
    #[test]
    fn the_standard_scrollbar_properties_stay_unset() {
        fn without_comments(css: &str) -> String {
            let mut out = String::new();
            let mut rest = css;
            while let Some(start) = rest.find("/*") {
                out.push_str(&rest[..start]);
                rest = match rest[start + 2..].find("*/") {
                    Some(end) => &rest[start + 2 + end + 2..],
                    None => "",
                };
            }
            out.push_str(rest);
            out
        }
        let code: String = include_str!("ui.rs")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let sources = [
            ("app.css", without_comments(APP_CSS)),
            ("theme.css", without_comments(THEME)),
            ("ui.rs", code),
        ];
        for part in ["width", "color"] {
            let property = format!("scrollbar-{part}");
            for (name, text) in &sources {
                assert!(!text.contains(&property), "{name} sets {property}");
            }
        }
        assert!(
            sources[0].1.contains("::-webkit-scrollbar-thumb"),
            "app.css draws the thumb"
        );
    }

    /// The bunny in the tray's menu, composited onto the card colour the same
    /// way as the cat, and held to it the same way: a mismatch would show as a
    /// rectangle in the corner of the menu.
    #[test]
    fn the_tray_bunny_matches_the_card_surface() {
        const SURFACE: [u8; 3] = [0x1e, 0x16, 0x29];
        assert!(
            THEME.contains("--color-surface: #1e1629;"),
            "the card surface changed colour: remake tray-bunny.jpg on the new value,              then change SURFACE here"
        );

        let mut decoder = jpeg_decoder::Decoder::new(TRAY_BUNNY);
        let pixels = decoder.decode().expect("tray-bunny.jpg decodes");
        let info = decoder.info().expect("decoded, so described");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        let (width, height) = (usize::from(info.width), usize::from(info.height));
        let at = |x: usize, y: usize| &pixels[(y * width + x) * 3..(y * width + x) * 3 + 3];
        let edges = (0..height)
            .flat_map(|y| [at(0, y), at(width - 1, y)])
            .chain((0..width).flat_map(|x| [at(x, 0), at(x, height - 1)]));
        for pixel in edges {
            for (got, want) in pixel.iter().zip(SURFACE) {
                assert!(
                    got.abs_diff(want) <= 3,
                    "edge pixel {pixel:?} is not the card's {SURFACE:?}"
                );
            }
        }

        assert_eq!(
            APP_CSS.matches(TRAY_BUNNY_URL).count(),
            1,
            "app.css names the bunny once"
        );
        assert!(
            !STYLES.contains(TRAY_BUNNY_URL),
            "the bunny was not inlined"
        );
        assert!(
            SOURCE.contains("class: \"tray-card\""),
            "the menu lost its card"
        );
    }

    /// build.rs has to get bliss.rc into the executable; without it the window
    /// quietly falls back to the Dioxus logo and Explorer to the generic
    /// application icon. `LoadImageW` resizes whatever frame it finds, so this
    /// can only tell that resource 1 is there — which frames it holds is the
    /// test above.
    #[cfg(windows)]
    #[test]
    fn the_icon_is_compiled_in() {
        assert!(
            app_icon(None).is_some(),
            "no icon resource 1 in this executable: build.rs did not compile bliss.rc"
        );
    }

    /// The scene's sky was shifted to the card's colour when the file was made,
    /// and app.css names the file so that `STYLES` can inline it.
    #[test]
    fn the_scene_matches_the_card_surface() {
        const SURFACE: [u8; 3] = [0x1e, 0x16, 0x29];
        assert!(
            THEME.contains("--color-surface: #1e1629;"),
            "the card surface changed colour: remake status-card.jpg with its edges \
             at the new value, then change SURFACE here"
        );

        // The picture itself: its left and top edges have to be that colour, or
        // they show as a rectangle on the card.
        let mut decoder = jpeg_decoder::Decoder::new(SCENE);
        let pixels = decoder.decode().expect("status-card.jpg decodes");
        let info = decoder.info().expect("decoded, so described");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        let (width, height) = (usize::from(info.width), usize::from(info.height));
        let at = |x: usize, y: usize| &pixels[(y * width + x) * 3..(y * width + x) * 3 + 3];
        for pixel in (0..height)
            .map(|y| at(0, y))
            .chain((0..width).map(|x| at(x, 0)))
        {
            for (got, want) in pixel.iter().zip(SURFACE) {
                assert!(
                    got.abs_diff(want) <= 3,
                    "edge pixel {pixel:?} is not the card's {SURFACE:?}"
                );
            }
        }

        assert_eq!(
            APP_CSS.matches(SCENE_URL).count(),
            1,
            "app.css names the scene once"
        );
        assert!(!STYLES.contains(SCENE_URL), "the scene was not inlined");
        // base64 of a JPEG's first three bytes, FF D8 FF.
        assert!(STYLES.contains("url(data:image/jpeg;base64,/9j/"));
        assert!(
            SOURCE.contains("class: \"status-card\""),
            "the status card lost its class"
        );
    }

    /// The cat at the foot of the rail. Its transparent ground was composited
    /// onto the card colour when the file was made, so all four of its edges
    /// have to be that colour, or it shows on the rail as a square.
    #[test]
    fn the_rail_cat_matches_the_card_surface() {
        const SURFACE: [u8; 3] = [0x1e, 0x16, 0x29];
        assert!(
            THEME.contains("--color-surface: #1e1629;"),
            "the card surface changed colour: remake rail-cat.jpg on the new value, \
             then change SURFACE here"
        );

        let mut decoder = jpeg_decoder::Decoder::new(RAIL_CAT);
        let pixels = decoder.decode().expect("rail-cat.jpg decodes");
        let info = decoder.info().expect("decoded, so described");
        assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
        let (width, height) = (usize::from(info.width), usize::from(info.height));
        let at = |x: usize, y: usize| &pixels[(y * width + x) * 3..(y * width + x) * 3 + 3];
        let edges = (0..height)
            .flat_map(|y| [at(0, y), at(width - 1, y)])
            .chain((0..width).flat_map(|x| [at(x, 0), at(x, height - 1)]));
        for pixel in edges {
            for (got, want) in pixel.iter().zip(SURFACE) {
                assert!(
                    got.abs_diff(want) <= 3,
                    "edge pixel {pixel:?} is not the card's {SURFACE:?}"
                );
            }
        }

        assert_eq!(
            APP_CSS.matches(RAIL_CAT_URL).count(),
            1,
            "app.css names the cat once"
        );
        assert!(!STYLES.contains(RAIL_CAT_URL), "the cat was not inlined");
        assert!(
            SOURCE.contains("class: \"rail-cat\""),
            "the rail lost its cat"
        );
    }

    /// The font is inlined like the scene, and its licence stays beside it: the
    /// SIL Open Font License asks that the licence travel with the font.
    #[test]
    fn the_font_is_inlined_and_licensed() {
        assert_eq!(
            THEME.matches(FONT_URL).count(),
            1,
            "theme.css names the font once"
        );
        assert!(!THEME_STYLES.contains(FONT_URL), "the font was not inlined");
        // base64 of a woff2 file's signature, "wOF2".
        assert!(THEME_STYLES.contains("url(data:font/woff2;base64,d09GMg"));
        assert!(THEME.contains("--font-body: \"Comfortaa\""));
        const LICENCE: &str = include_str!("../assets/OFL-Comfortaa.txt");
        assert!(LICENCE.contains("SIL Open Font License"));
    }

    /// A table's text columns are `minmax(0, …fr)`, never a bare `fr`. A bare
    /// track is at least as wide as its longest unbreakable word, and every
    /// row is a grid of its own: a long host name moved its own row's columns
    /// and no other's, and a narrow window lost the table's order.
    #[test]
    fn table_columns_do_not_grow_with_their_text() {
        let measured = ServerColumns::new(900.0, Some(400.0), None)
            .expect("room for the columns")
            .css();
        for columns in [CONNECTION_COLUMNS, SERVER_COLUMNS, measured.as_str()] {
            let tracks = columns
                .strip_prefix("grid-template-columns:")
                .and_then(|rest| rest.split(';').next())
                .expect("a grid-template-columns declaration");
            for track in tracks.split(' ') {
                assert!(
                    !track.contains("fr") || track.starts_with("minmax(0,"),
                    "{track:?} in {columns:?} grows with its longest word"
                );
            }
        }
    }

    /// However the server list's columns are dragged, the row fits the list:
    /// name, address and protocol never take more than the room there is, and
    /// neither text column goes below its least. With nothing dragged, the
    /// name and the address share as the fractions did.
    #[test]
    fn dragged_server_columns_still_fit_the_list() {
        for width in [520.0, 700.0, 900.0, 1400.0, 2600.0] {
            let room = width - SERVER_PAD * 2.0 - SERVER_FIXED;
            for name in [None, Some(-500.0), Some(10.0), Some(300.0), Some(5000.0)] {
                for kind in [None, Some(-50.0), Some(40.0), Some(200.0), Some(5000.0)] {
                    let c = ServerColumns::new(width, name, kind).expect("room");
                    assert!(
                        (c.name + c.addr + c.kind - room).abs() < 0.01,
                        "{width} {name:?} {kind:?}: {c:?}"
                    );
                    assert!(
                        c.name >= MIN_TEXT_COLUMN && c.addr >= MIN_TEXT_COLUMN,
                        "{c:?}"
                    );
                    assert!(c.kind >= 70.0, "{c:?}");
                }
            }
        }
        let c = ServerColumns::new(1000.0, None, None).expect("room");
        assert_eq!(c.kind, SERVER_KIND);
        assert!((c.name / c.addr - 1.2 / 1.4).abs() < 0.01, "{c:?}");
        // Too narrow for a table: the fractions draw it, as before measuring.
        assert_eq!(ServerColumns::new(300.0, None, None), None);
    }

    /// A wider column shows more of a name, and a name that fits is whole.
    #[test]
    fn a_wider_column_cuts_a_name_later() {
        let name = "Netherlands · Amsterdam · VPN premium 10 Gbit/s";
        let narrow = elide(name, letters_in(150.0, 14.5));
        let wide = elide(name, letters_in(450.0, 14.5));
        assert!(
            narrow.ends_with('\u{2026}') && narrow.chars().count() < 20,
            "{narrow}"
        );
        assert_eq!(wide, name);
    }

    /// The inline radii are literals because the token form does not survive
    /// being rewritten (see the note at the top of this file), which means the
    /// numbers are written down twice. This is the thing that keeps the second
    /// copy honest: change a token and this fails rather than the window
    /// quietly drifting away from the design system.
    #[test]
    fn radius_literals_match_the_design_tokens() {
        for (token, literal) in [
            ("--radius-sm", "4px"),
            ("--radius-md", "8px"),
            // The rule editor's corners, in app.css.
            ("--radius-lg", "14px"),
        ] {
            let declaration = format!("{token}: {literal};");
            assert!(
                THEME.contains(&declaration),
                "theme.css no longer says {declaration:?} — the inline radii in this file \
                 still use {literal} and have to be updated with it"
            );
        }

        // And the forms that do not survive must not creep back in, anywhere in
        // this file. The first version of this check stopped at this test
        // module, which left the log pane after it unguarded, and matched exact
        // no-space spellings, so `border-radius: var(` passed. Now whitespace is
        // stripped before matching, the value after each shorthand is inspected
        // up to the next `;` or quote, and the marker is assembled at runtime so
        // this test's own source never matches it.
        // Comment lines are skipped: prose about the rule — this comment
        // included — is not a style and must not trip it.
        let code: String = SOURCE
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(str::chars)
            .filter(|c| !c.is_whitespace())
            .collect();
        let variable = ["var", "("].concat();
        for shorthand in [
            "border-radius:",
            "background:",
            "border:",
            "border-top:",
            "border-right:",
            "border-bottom:",
            "border-left:",
        ] {
            for (at, _) in code.match_indices(shorthand) {
                let rest = &code[at + shorthand.len()..];
                let value = &rest[..rest.find([';', '"']).unwrap_or(rest.len())];
                assert!(
                    !value.contains(&variable),
                    "`{shorthand}{value}` puts a variable in a shorthand, which the renderer \
                     destroys when the style is rewritten"
                );
            }
        }
    }

    /// The rail's look must come from its class alone.
    ///
    /// This bug was reported three times and declared fixed twice. The second
    /// "fix" never reached the file — a script aborted before writing and the
    /// change was then verified against a replica instead of against this code.
    /// So the check is on the source of `NavItem` itself: its inline style may
    /// not interpolate anything, because anything interpolated can vary by
    /// state, and a state-dependent inline property is restored by the renderer
    /// forever.
    #[test]
    fn the_rail_item_style_does_not_vary_by_state() {
        let body = SOURCE
            .split_once("fn NavItem(")
            .expect("NavItem exists")
            .1
            .split_once("/// Icons follow the label's colour")
            .expect("NavItem is followed by nav_tint")
            .0;
        let style = body
            .split_once("style: \"")
            .expect("NavItem has an inline style")
            .1
            .split_once("\",")
            .expect("the style string ends")
            .0;

        assert!(
            !style.contains('{'),
            "NavItem's inline style interpolates a value: {style:?}"
        );
        for state_look in ["box-shadow", "background", "color:"] {
            assert!(
                !style.contains(state_look),
                "{state_look} belongs in .nav-item.active, not inline: {style:?}"
            );
        }
    }

    /// Up to the default window nothing is scaled; past it the page grows, in
    /// steps, and never so much that a maximised window looks like a poster.
    #[test]
    fn the_page_grows_with_the_window_only_past_the_default_size() {
        assert_eq!(zoom_for(1040.0, 660.0), 1.0);
        assert_eq!(zoom_for(880.0, 560.0), 1.0);
        assert_eq!(zoom_for(4000.0, 660.0), 1.0, "the smaller ratio decides");
        assert_eq!(zoom_for(0.0, 0.0), 1.0);
        // Maximised on the 2560 by 1440 monitor this was reported from.
        let maximised = zoom_for(2560.0, 1392.0);
        assert!((1.3..=1.6).contains(&maximised), "{maximised}");

        let mut last = 1.0;
        for step in 0..200 {
            let step = f64::from(step);
            let zoom = zoom_for(1040.0 + 20.0 * step, 660.0 + 13.0 * step);
            assert!(zoom >= last, "never smaller in a larger window");
            assert!(
                ((zoom * 20.0).round() - zoom * 20.0).abs() < 1e-9,
                "in steps of 5%: {zoom}"
            );
            last = zoom;
        }
        assert!(last <= 1.6, "{last}");
    }

    /// Clickable things take their pointer from app.css, and an inline cursor
    /// outranks any class: a `cursor:default` left inline on the action cards
    /// is why hovering them changed nothing. Only the window's own chrome, the
    /// drag strip and its three buttons, keeps the arrow inline.
    #[test]
    fn only_the_window_chrome_sets_a_cursor_inline() {
        let marker = ["cursor", ":"].concat();
        let inline: Vec<&str> = SOURCE
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| line.contains(&marker))
            .collect();
        assert_eq!(inline.len(), 2, "{inline:#?}");
    }

    #[test]
    fn the_table_searches_and_sorts_what_it_shows() {
        let row = |id: u64, target: &str, process: Option<&str>, down: u64, up: u64| {
            bliss_core::Snapshot {
                id,
                target: target.to_owned(),
                route: bliss_core::Route::Proxy,
                rule: "по умолчанию".to_owned(),
                process: process.map(str::to_owned),
                opened_at: jiff::Timestamp::UNIX_EPOCH,
                closed_at: None,
                up,
                down,
            }
        };
        let rows = [
            row(3, "a.example:443", Some("Chrome.exe"), 10, 500),
            row(2, "b.example:443", Some("telegram.exe"), 900, 5),
            row(1, "c.example:443", None, 50, 50),
        ];
        let mut rows = rows;
        rows[1].rule = "Локальная сеть".to_owned();
        let ids =
            |shown: Vec<&bliss_core::Snapshot>| shown.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(table_rows(&rows, "", None, Sort::Newest)), [3, 2, 1]);
        assert_eq!(
            ids(table_rows(&rows, "CHROME", None, Sort::Newest)),
            [3],
            "any case"
        );
        assert_eq!(
            ids(table_rows(&rows, " example ", None, Sort::DownMost)),
            [2, 1, 3]
        );
        assert_eq!(ids(table_rows(&rows, "", None, Sort::DownLeast)), [3, 1, 2]);
        assert_eq!(ids(table_rows(&rows, "", None, Sort::UpMost)), [3, 1, 2]);
        assert_eq!(ids(table_rows(&rows, "", None, Sort::UpLeast)), [2, 1, 3]);
        assert!(table_rows(&rows, "nothing", None, Sort::Newest).is_empty());

        // A rule is chosen from its list, not typed: its name is not in the
        // table, and a search that matched it found rows showing no reason.
        assert!(table_rows(&rows, "Локальная", None, Sort::Newest).is_empty());
        let local = Some("Локальная сеть");
        assert_eq!(ids(table_rows(&rows, "", local, Sort::Newest)), [2]);
        assert!(table_rows(&rows, "chrome", local, Sort::Newest).is_empty());
    }

    #[test]
    fn the_rule_list_offers_every_rule_with_its_count() {
        let row = |id: u64, rule: &str| bliss_core::Snapshot {
            id,
            target: "a.example:443".to_owned(),
            route: bliss_core::Route::Proxy,
            rule: rule.to_owned(),
            process: None,
            opened_at: jiff::Timestamp::UNIX_EPOCH,
            closed_at: None,
            up: 0,
            down: 0,
        };
        let rule = |position: usize, name: &str| crate::engine::RuleView {
            position,
            name: name.to_owned(),
            action: Action::Direct,
            conditions: Vec::new(),
            enabled: true,
        };
        let routing = crate::engine::RoutingView {
            rules: vec![rule(1, "Реклама"), rule(2, "Локальная сеть")],
            ..Default::default()
        };
        let rows = [
            row(1, "по умолчанию"),
            row(2, "Локальная сеть"),
            row(3, "по умолчанию"),
            row(4, "удалённое"),
        ];
        assert_eq!(
            rule_choices(&routing, &rows),
            [
                ("Реклама".to_owned(), 0),
                ("Локальная сеть".to_owned(), 1),
                ("по умолчанию".to_owned(), 2),
                // A rule deleted since still decided a connection on screen.
                ("удалённое".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn subscription_servers_are_grouped_and_open_where_the_server_in_use_is() {
        use crate::engine::ServerRow;
        let server = |tag: &str, group: Option<(&str, &str)>, current: bool| ServerRow {
            tag: tag.to_owned(),
            current,
            group_id: group.map(|(id, _)| id.to_owned()),
            group: group.map(|(_, name)| name.to_owned()),
            ..ServerRow::default()
        };
        let one = Some(("s1", "Провайдер"));
        let two = Some(("s2", "Второй"));
        let servers = [
            server("mine", None, false),
            server("a", one, false),
            server("b", one, true),
            server("c", two, false),
            server("also mine", None, false),
        ];

        let sections = server_sections(&servers, &Default::default());
        let shape: Vec<(Option<&str>, Vec<usize>, bool)> = sections
            .iter()
            .map(|s| {
                (
                    s.group.as_ref().map(|(id, _)| id.as_str()),
                    s.rows.clone(),
                    s.open,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                (None, vec![0, 4], true),
                (Some("s1"), vec![1, 2], true),
                (Some("s2"), vec![3], false),
            ]
        );

        // Opened or closed by hand, a group stays that way.
        let chosen =
            std::collections::HashMap::from([("s1".to_owned(), false), ("s2".to_owned(), true)]);
        let sections = server_sections(&servers, &chosen);
        assert!(!sections[1].open && sections[1].has_current);
        assert!(sections[2].open);
    }

    #[test]
    fn the_time_since_an_update_reads_like_speech() {
        assert_eq!(ago(1_000, 1_030), "только что");
        assert_eq!(ago(1_000, 1_000 + 5 * 60), "5 минут назад");
        assert_eq!(ago(1_000, 1_000 + 3600), "1 час назад");
        assert_eq!(ago(1_000, 1_000 + 3 * 86_400), "3 дня назад");
        assert_eq!(ago(5_000, 1_000), "только что", "a clock that moved back");
    }

    #[test]
    fn russian_plurals_agree() {
        let count = |n| plural(n, "соединение", "соединения", "соединений");
        assert_eq!(count(1), "1 соединение");
        assert_eq!(count(2), "2 соединения");
        assert_eq!(count(4), "4 соединения");
        assert_eq!(count(5), "5 соединений");
        assert_eq!(count(0), "0 соединений");
        // The teens are the exception that catches naive implementations.
        assert_eq!(count(11), "11 соединений");
        assert_eq!(count(12), "12 соединений");
        assert_eq!(count(14), "14 соединений");
        assert_eq!(count(21), "21 соединение");
        assert_eq!(count(22), "22 соединения");
        assert_eq!(count(111), "111 соединений");
        assert_eq!(count(121), "121 соединение");
    }

    /// The line bends between samples and never beyond them: every point of
    /// every segment lies between the two samples it joins, so it neither dips
    /// under zero beside a burst nor rises over a peak.
    #[test]
    fn the_chart_line_never_overshoots_a_sample() {
        let rates = [
            0u64, 0, 512_000, 0, 30_000, 31_000, 4_000_000, 3_900_000, 0, 0, 1,
        ];
        let points: Vec<(f64, f64)> = rates
            .iter()
            .enumerate()
            .map(|(i, &rate)| (i as f64 * CHART_STEP, rate as f64 / 1024.0))
            .collect();
        let segments = monotone_segments(&points);
        assert_eq!(segments.len(), points.len() - 1);
        for (i, [a, b, end]) in segments.iter().enumerate() {
            let start = points[i];
            assert_eq!(*end, points[i + 1]);
            let (low, high) = (start.1.min(end.1), start.1.max(end.1));
            for step in 0..=100 {
                let t = f64::from(step) / 100.0;
                let u = 1.0 - t;
                let y = u * u * u * start.1
                    + 3.0 * u * u * t * a.1
                    + 3.0 * u * t * t * b.1
                    + t * t * t * end.1;
                assert!(
                    y >= low - 1e-9 && y <= high + 1e-9,
                    "segment {i} at {t}: {y} outside {low}..{high}"
                );
            }
        }
        let (line, area) = chart_paths(&rates);
        assert!(line.starts_with("M0.00,0.00C"), "{line}");
        assert!(area.starts_with(&line) && area.ends_with("L100.00,0L0.00,0Z"));
    }

    /// A new sample starts the glide again; the tab's first frame and an idle
    /// chart stand still; and two samples in a row never share an animation,
    /// or the second would not restart it.
    #[test]
    fn each_new_sample_restarts_the_glide() {
        assert_eq!(
            glide_class(7, 7, false),
            "glide still",
            "opened on this sample"
        );
        assert_eq!(glide_class(9, 7, true), "glide still", "nothing to draw");
        assert_ne!(glide_class(8, 7, false), glide_class(9, 7, false));
        assert_ne!(glide_class(9, 7, false), glide_class(10, 7, false));
        assert!(glide_class(8, 7, false).starts_with("glide "));
    }

    /// The layer is one sample wider than the chart on each side, and a glide
    /// is one sample of its own width over one tick. app.css writes all three
    /// as numbers, which must follow `CHART_SAMPLES` and `TICK`: a glide a
    /// little short or long makes the line jump back or forth on every sample.
    #[test]
    fn the_chart_glides_one_sample_per_tick() {
        let n = crate::engine::CHART_SAMPLES as f64;
        let width = format!("width: {:.1}%", 100.0 * (n - 1.0) / (n - 2.0));
        let glide = format!("translateX(-{:.7}%)", 100.0 / (n - 1.0));
        let tick = format!("{}ms steps(", crate::engine::TICK.as_millis());
        assert_eq!(APP_CSS.matches(&width).count(), 1, "{width}");
        assert_eq!(
            APP_CSS.matches(&glide).count(),
            3,
            "{glide}: two glides and where they end"
        );
        assert_eq!(APP_CSS.matches(&tick).count(), 2, "{tick}");
    }

    /// A digit's box is as wide as the widest digit this font draws, and the
    /// numbers that change under the eye go through `Ticking`.
    ///
    /// Comfortaa has no `tnum`, so every `font-variant-numeric:tabular-nums`
    /// in this file is decoration; the box is what actually holds the line
    /// still. Narrow the box and the clock starts walking again.
    #[test]
    fn the_numbers_that_tick_stand_still() {
        assert!(
            APP_CSS.contains(&format!(
                ".figure {{ display: inline-block; width: {FIGURE_EM}em"
            )),
            "the box in app.css no longer matches the widest digit measured in ui.rs"
        );

        // The clock and the metrics are the two that change twice a second.
        let card = SOURCE
            .split_once("fn StatusCards(")
            .expect("StatusCards exists")
            .1
            .split_once("fn AddServerCard(")
            .expect("AddServerCard follows it")
            .0;
        assert!(
            card.contains("Ticking { text: frame.uptime.clone() }"),
            "the clock is back to plain text and will walk its line again"
        );
        let row = SOURCE
            .split_once("fn StatRow(")
            .expect("StatRow exists")
            .1
            .split_once("fn Ticking(")
            .expect("Ticking follows it")
            .0;
        assert!(row.contains("Ticking { text: value }"), "{row}");
    }

    /// The switch under the ring keeps its look in classes. An inline
    /// property present in one state and absent in another is restored by
    /// Dioxus and then stays forever — the bug that made the rail's highlight
    /// stick to every item ever clicked, reported three times.
    #[test]
    fn the_tun_switch_style_does_not_vary_by_state() {
        let body = SOURCE
            .split_once("fn TunSwitch(")
            .expect("TunSwitch exists")
            .1
            .split_once("/// What the connection screen shows before there is a server")
            .expect("TunSwitch is followed by AddServerCard")
            .0;
        let style = body
            .split_once("style: \"")
            .expect("TunSwitch has an inline style")
            .1
            .split_once("\",")
            .expect("the style string ends")
            .0;

        assert!(
            !style.contains('{'),
            "the switch's inline style interpolates a value: {style:?}"
        );
        for state_look in ["transform", "box-shadow", "background", "color:"] {
            assert!(
                !style.contains(state_look),
                "{state_look} belongs in .tun-switch.on, not inline: {style:?}"
            );
        }
    }

    /// The knob lands exactly on the other half. Its travel is not a number
    /// anyone can see is right: it is the pill's width less its padding,
    /// halved, and a pill made wider without moving it would leave the knob
    /// short of the icon it is supposed to sit under.
    #[test]
    fn the_switch_knob_travels_exactly_one_half() {
        let number = |after: &str, within: &str| -> f64 {
            let tail = APP_CSS
                .split_once(within)
                .unwrap_or_else(|| panic!("app.css has {within}"))
                .1;
            let value = tail
                .split_once(after)
                .unwrap_or_else(|| panic!("{within} sets {after}"))
                .1;
            value
                .trim_start()
                .trim_start_matches(|c: char| !c.is_ascii_digit())
                .split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()
                .expect("a number")
                .parse()
                .expect("a number")
        };

        let width = number("width:", ".tun-switch {");
        let padding = number("padding:", ".tun-switch {");
        let knob = number("width:", ".tun-knob {");
        let travel = number("translateX(", ".tun-switch.on .tun-knob {");

        assert_eq!(knob, (width - 2.0 * padding) / 2.0, "the knob is one half");
        assert_eq!(travel, knob, "and it moves by exactly its own width");
    }

    /// Bands, not a gradient — and the distinctions that have to survive
    /// someone tuning the thresholds later.
    #[test]
    fn a_ping_reads_the_same_way_at_a_glance_every_time() {
        // Inside a band the colour does not move; crossing one, it does.
        assert_eq!(ping_tone(Ping::Ok(0)), ping_tone(Ping::Ok(119)));
        assert_ne!(ping_tone(Ping::Ok(119)), ping_tone(Ping::Ok(120)));
        assert_ne!(ping_tone(Ping::Ok(299)), ping_tone(Ping::Ok(300)));

        // However slow, a server that answered is not a server that did not.
        for ms in [0, 119, 120, 299, 300, 4_000] {
            assert_ne!(ping_tone(Ping::Ok(ms)), ping_tone(Ping::Down));
        }

        // And "not measured" is a third thing again: reading it as failure
        // would condemn every server in the list for the second before the
        // first sweep lands.
        assert_ne!(ping_tone(Ping::Unknown), ping_tone(Ping::Down));
        assert_ne!(ping_text(Ping::Unknown), ping_text(Ping::Down));
        assert_eq!(ping_text(Ping::Ok(42)), "42 мс");
    }
}

/// The live log, as in mockup 1b.
///
/// It is the thing that makes the window look like it is doing something, and
/// the only place a REALITY failure is visible at all — the protocol's whole
/// design is that a rejected client cannot tell it was rejected.
///
/// Reads the buffer directly rather than taking it as a prop: piping several
/// hundred lines through every frame, twice a second, would cost more than the
/// pane is worth. What it does take is the buffer's `revision`, and that is not
/// decoration: a component is re-run by its parent only when its props change,
/// so with none at all the pane drew the buffer once and after that only when
/// its own copy note changed — a "live" log that moved when clicked.
#[component]
fn LogStrip(revision: u64) -> Element {
    // Compared, not read: a new line is a new value, and that is the point.
    let _ = revision;
    /// Enough to scroll back through without asking the renderer to diff the
    /// whole buffer twice a second.
    const SHOWN: usize = 200;

    let lines = crate::logs::history(SHOWN);
    // What the last copy did. Stays until the next one rather than fading on a
    // timer: a confirmation that disappears while you are still looking at it
    // leaves you unsure whether you saw it.
    let mut copied = use_signal(String::new);
    let note = copied();

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:9px",
            div { style: "display:flex;align-items:center;gap:10px",
                span { style: "font:700 14px/1 var(--font-heading)", "Журнал" }
                span { class: "tag tag-accent", style: "font-size:11px;padding:2px 8px", "live" }
                span {
                    style: "margin-left:auto;font-size:12.5px;color:var(--color-neutral-600);\
                            min-width:0;overflow:hidden;white-space:nowrap",
                    if note.is_empty() { "строка копируется кликом" } else { "{note}" }
                }
                Button {
                    label: "Скопировать всё",
                    enabled: !lines.is_empty(),
                    onclick: move |_| {
                        // Oldest first, the other way round from the screen. A
                        // log is read downwards through time once it has been
                        // pasted somewhere, and the note says which way it went
                        // so the order is not a surprise.
                        let all = crate::logs::history(crate::logs::CAPACITY);
                        let text = all
                            .iter()
                            .rev()
                            .map(as_text)
                            .collect::<Vec<_>>()
                            .join("\n");
                        copied.set(if crate::clipboard::copy(&text) {
                            format!("скопировано {} — сверху старые", crate::clipboard::lines(all.len()))
                        } else {
                            "буфер обмена занят другой программой".to_owned()
                        });
                    },
                    icon: rsx! { crate::icons::Copy { size: 15, color: ACCENT } },
                }
            }
            div {
                style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;\
                        background-color:#120c1a;border-radius:4px;padding:9px 12px;\
                        display:flex;flex-direction:column;gap:4px;\
                        font-family:ui-monospace,Consolas,monospace;font-size:12.5px;line-height:1.5",
                if lines.is_empty() {
                    span { style: "color:var(--color-neutral-700)", "пока тихо" }
                }
                for (n, line) in lines.iter().enumerate() {
                    div {
                        key: "{n}",
                        class: "logline",
                        style: "display:flex;gap:10px;align-items:baseline;\
                                border-radius:3px;margin:0 -4px;padding:0 4px",
                        onclick: {
                            let text = as_text(line);
                            move |_| {
                                copied.set(if crate::clipboard::copy(&text) {
                                    format!("скопировано: {}", elide(&text, 40))
                                } else {
                                    "буфер обмена занят другой программой".to_owned()
                                });
                            }
                        },
                        span {
                            style: "color:var(--color-neutral-700);flex:none",
                            "{line.time}"
                        }
                        span {
                            style: "color:{line.level.colour()};width:46px;flex:none",
                            "{line.level.label()}"
                        }
                        // Wraps instead of being cut: a log line that runs off
                        // the edge is a log line you cannot read.
                        span {
                            style: "color:var(--color-neutral-300);min-width:0;word-break:break-word",
                            "{line.text}"
                        }
                    }
                }
            }
        }
    }
}

/// One table row as text.
///
/// Tab-separated, because what people do with a copied table is paste it
/// somewhere that understands columns — and because a rule name can contain
/// spaces, so spaces cannot be the separator.
fn row_as_text(row: &bliss_core::Snapshot) -> String {
    format!(
        "{}\t{}\t{}\t{}\t↓ {}\t↑ {}",
        row.target,
        row.process.as_deref().unwrap_or("—"),
        if row.closed_at.is_some() {
            "закрыто"
        } else {
            "открыто"
        },
        row.rule,
        bytes(row.down),
        bytes(row.up)
    )
}

/// One log line as text, which is what goes on the clipboard.
///
/// The level is padded so a pasted block lines up in whatever it is pasted
/// into — the same reason the pane gives the column a fixed width.
fn as_text(line: &crate::logs::Line) -> String {
    format!("{}  {:<5}  {}", line.time, line.level.label(), line.text)
}

// --- zoom -----------------------------------------------------------------

/// How much to scale the page for a window this size, in logical pixels.
///
/// The screens are laid out for the default window, 1040 by 660. Maximised on
/// a large monitor, the same cards stretched across twice the width with the
/// same 14-pixel text: a small window's content lost in a big one. So past the
/// default size the page grows with the window — by the smaller of the two
/// ratios, so neither direction overflows, and by only half of it, so more
/// room still shows more rather than the same things blown up. In steps of 5%,
/// so a live resize does not reflow the page at every pixel.
fn zoom_for(width: f64, height: f64) -> f64 {
    const BASE: (f64, f64) = (1040.0, 660.0);
    const GROWTH: f64 = 0.5;
    const MOST: f64 = 1.6;
    let ratio = (width / BASE.0).min(height / BASE.1);
    if !ratio.is_finite() || ratio <= 1.0 {
        return 1.0;
    }
    let zoom = (1.0 + (ratio - 1.0) * GROWTH).min(MOST);
    (zoom * 20.0).floor() / 20.0
}

/// Scales the page with the window; see `zoom_for`.
#[component]
fn WindowZoom() -> Element {
    let desktop = dioxus::desktop::window();
    let applied = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(1.0_f64)));
    use_hook({
        let desktop = desktop.clone();
        let applied = applied.clone();
        move || apply_zoom(&desktop, &applied)
    });
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        if let Event::WindowEvent {
            window_id,
            event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. },
            ..
        } = event
            && *window_id == desktop.window.id()
        {
            apply_zoom(&desktop, &applied);
        }
    });
    rsx! {}
}

fn apply_zoom(desktop: &dioxus::desktop::DesktopContext, applied: &std::cell::Cell<f64>) {
    let (width, height) = logical_size(&desktop.window);
    // Minimised, the window reports next to no size; keep the zoom it had.
    if width < 100.0 || height < 100.0 {
        return;
    }
    let zoom = zoom_for(width, height);
    if zoom != applied.get() {
        applied.set(zoom);
        desktop.set_zoom_level(zoom);
    }
}

/// The window's inside, in logical pixels.
fn logical_size(window: &Window) -> (f64, f64) {
    let size = window.inner_size().to_logical::<f64>(window.scale_factor());
    (size.width, size.height)
}

/// The page's size in CSS pixels: the window's, divided by its zoom.
fn viewport() -> (f64, f64) {
    let desktop = dioxus::desktop::window();
    let (width, height) = logical_size(&desktop.window);
    let zoom = zoom_for(width, height);
    (width / zoom, height / zoom)
}

// --- shared pieces ----------------------------------------------------------

/// The line about what the last action did. Keyed by its id, so a new line
/// fades in even when it replaces one still showing.
#[component]
fn ToastLine(toast: Option<crate::engine::Toast>) -> Element {
    let Some(toast) = toast else {
        return rsx! {};
    };
    rsx! {
        div { key: "{toast.id}", class: if toast.bad { "toast bad" } else { "toast" }, "{toast.text}" }
    }
}

/// A small square button with an icon and no label, for the tools on a row.
///
/// It keeps its click to itself: the row under it has a click of its own.
#[component]
fn IconButton(
    icon: Element,
    title: String,
    enabled: bool,
    #[props(default)] danger: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let class = match (enabled, danger) {
        (false, _) => "icon-btn disabled",
        (true, true) => "icon-btn danger",
        (true, false) => "icon-btn",
    };
    rsx! {
        div {
            class: "{class}",
            title: "{title}",
            onclick: move |e: MouseEvent| {
                e.stop_propagation();
                if enabled {
                    onclick.call(e);
                }
            },
            {icon}
        }
    }
}

/// The actions a choice offers. The fourth only once there is a VPN to send
/// connections to — or when it is what is chosen already, so a rule that sends
/// there after the VPN's profile was removed still says where it sends.
fn actions(vpn: bool, chosen: Action) -> Vec<Action> {
    let mut all = vec![Action::Proxy, Action::Direct, Action::Block];
    if vpn || chosen == Action::Vpn {
        all.push(Action::Vpn);
    }
    all
}

fn action_key(action: Action) -> &'static str {
    match action {
        Action::Proxy => "proxy",
        Action::Direct => "direct",
        Action::Block => "block",
        Action::Vpn => "vpn",
    }
}

fn action_phrase(action: Action) -> &'static str {
    match action {
        Action::Proxy => "Через прокси",
        Action::Direct => "Напрямую",
        Action::Block => "Блокировать",
        Action::Vpn => "Через VPN",
    }
}

/// The class of one of three action choices: which action, and whether it is
/// the chosen one. State by class, never inline (see the note at the top).
fn choice_class(base: &str, on: bool, action: Action) -> String {
    let on = if on { " on" } else { "" };
    format!("{base} {}{on}", action_key(action))
}

// --- the connections table ----------------------------------------------------

/// The connections the table shows: those matching the search and decided by
/// the chosen rule, in the order asked for. A stable sort, so rows with equal
/// traffic keep the engine's newest-first order.
fn table_rows<'a>(
    rows: &'a [bliss_core::Snapshot],
    query: &str,
    rule: Option<&str>,
    sort: Sort,
) -> Vec<&'a bliss_core::Snapshot> {
    let query = query.trim().to_lowercase();
    let mut shown: Vec<&bliss_core::Snapshot> = rows
        .iter()
        .filter(|row| rule.is_none_or(|rule| row.rule == rule))
        .filter(|row| {
            // Only what the table shows. A rule's name is not in it, so the
            // search used to match rows for a reason nobody could see.
            query.is_empty()
                || row.target.to_lowercase().contains(&query)
                || row
                    .process
                    .as_deref()
                    .is_some_and(|process| process.to_lowercase().contains(&query))
        })
        .collect();
    match sort {
        Sort::Newest => {}
        Sort::DownMost => shown.sort_by_key(|row| std::cmp::Reverse(row.down)),
        Sort::DownLeast => shown.sort_by_key(|row| row.down),
        Sort::UpMost => shown.sort_by_key(|row| std::cmp::Reverse(row.up)),
        Sort::UpLeast => shown.sort_by_key(|row| row.up),
    }
    shown
}

/// A column header that orders the table by its traffic: most first, then
/// least first, then back to the engine's own order.
#[component]
fn SortHeader(label: &'static str, most: Sort, least: Sort) -> Element {
    let sort = TABLE_SORT();
    let (mark, tip) = if sort == most {
        (" ↓", "сначала больше всего; ещё клик — сначала меньше")
    } else if sort == least {
        (" ↑", "сначала меньше всего; ещё клик — как было")
    } else {
        ("", "упорядочить по трафику")
    };
    rsx! {
        span {
            class: if sort == most || sort == least { "sort on" } else { "sort" },
            style: "text-align:right",
            title: "{tip}",
            onclick: move |_| {
                *TABLE_SORT.write() = if sort == most {
                    least
                } else if sort == least {
                    Sort::Newest
                } else {
                    most
                };
            },
            "{label}{mark}"
        }
    }
}

fn route_short(route: bliss_core::Route) -> &'static str {
    match route {
        bliss_core::Route::Proxy => "прокси",
        bliss_core::Route::Direct => "напрямую",
        bliss_core::Route::Blocked => "блок",
        bliss_core::Route::Vpn => "VPN",
    }
}

fn route_word(route: bliss_core::Route) -> &'static str {
    match route {
        bliss_core::Route::Proxy => "через прокси",
        bliss_core::Route::Direct => "напрямую",
        bliss_core::Route::Blocked => "заблокировано",
        bliss_core::Route::Vpn => "через VPN",
    }
}

fn row_menu(row: &bliss_core::Snapshot, x: f64, y: f64) -> RowMenu {
    let host = bliss_core::Target::parse_authority(&row.target)
        .map(|target| target.host())
        .unwrap_or_else(|_| row.target.clone());
    RowMenu {
        x,
        y,
        target: row.target.clone(),
        host,
        process: row.process.clone(),
        route: row.route,
        rule: row.rule.clone(),
        text: row_as_text(row),
    }
}

fn subject_label(subject: &Subject) -> String {
    match subject {
        Subject::Program(name) => format!("программа · {}", elide(name, 22)),
        Subject::Site(site) => format!("сайт · {}", elide(site, 26)),
        Subject::Address(ip) => format!("адрес · {}", elide(ip, 26)),
    }
}

/// What a right-click on a connection offers: the three routes for its
/// program, the three for its site, and a copy of the row.
#[component]
fn RowMenuView(menu: RowMenu, mut copied: Signal<String>, vpn: bool) -> Element {
    let controller = use_context::<Controller>();
    // A connection that named an address has no site; the address stands in.
    let place = match crate::rule_text::site(&menu.host) {
        Some(site) => Subject::Site(site),
        None => Subject::Address(
            menu.host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
        ),
    };
    let subjects: Vec<Subject> = menu
        .process
        .clone()
        .map(Subject::Program)
        .into_iter()
        .chain(std::iter::once(place))
        .collect();

    // Kept inside the window: opened upwards when it would run off the bottom.
    let (width, height) = viewport();
    let groups = subjects.len() as f64;
    let lines = actions(vpn, Action::Proxy).len() as f64;
    let tall = 12.0 + 27.0 + groups * (11.0 + 27.0 + lines * 33.0) + 11.0 + 33.0;
    let left = menu.x.min(width - 256.0).max(4.0);
    let top = if menu.y + tall > height - 6.0 {
        (menu.y - tall).max(4.0)
    } else {
        menu.y
    };
    let target = menu.target.clone();
    let process = menu.process.clone();
    let text = menu.text.clone();

    rsx! {
        div {
            class: "menu-veil",
            onclick: move |_| *ROW_MENU.write() = None,
            oncontextmenu: move |e: MouseEvent| {
                e.prevent_default();
                *ROW_MENU.write() = None;
            },
        }
        div { class: "menu", style: "left:{left}px;top:{top}px",
            div { class: "menu-title",
                "сейчас {route_word(menu.route)} · {elide(&menu.rule, 20)}"
            }
            for subject in subjects {
                div { key: "{subject.value()}",
                    div { class: "menu-sep" }
                    div { class: "menu-title", "{subject_label(&subject)}" }
                    for action in actions(vpn, Action::Proxy) {
                        div {
                            key: "{action_key(action)}",
                            class: "menu-item",
                            onclick: {
                                let controller = controller.clone();
                                let subject = subject.clone();
                                let target = target.clone();
                                let process = process.clone();
                                move |_| {
                                    controller.send(Command::RouteOne {
                                        subject: subject.clone(),
                                        action,
                                        target: target.clone(),
                                        process: process.clone(),
                                    });
                                    *ROW_MENU.write() = None;
                                }
                            },
                            span { class: "menu-dot {action_key(action)}" }
                            "{action_phrase(action)}"
                        }
                    }
                }
            }
            div { class: "menu-sep" }
            div {
                class: "menu-item",
                onclick: move |_| {
                    copied.set(if crate::clipboard::copy(&text) {
                        format!("скопировано: {}", elide(&text, 44))
                    } else {
                        "буфер обмена занят другой программой".to_owned()
                    });
                    *ROW_MENU.write() = None;
                },
                crate::icons::Copy { size: 14, color: MUTED }
                "Скопировать строку"
            }
        }
    }
}

/// The log under the connections table, and the bar that shares the height
/// between them.
#[component]
fn LogDock(revision: u64) -> Element {
    use dioxus::prelude::dioxus_elements::input_data::MouseButton;

    // Where a drag began: the pointer's height on the page, and the log's.
    let mut drag = use_signal(|| None::<(f64, f64)>);
    let height = LOG_HEIGHT();
    rsx! {
        div {
            class: if drag().is_some() { "splitter dragging" } else { "splitter" },
            title: "Потяните, чтобы изменить высоту журнала. Двойной клик — как было",
            onmousedown: move |e: MouseEvent| {
                e.prevent_default();
                drag.set(Some((e.client_coordinates().y, LOG_HEIGHT())));
            },
            ondoubleclick: move |_| *LOG_HEIGHT.write() = DEFAULT_LOG_HEIGHT,
        }
        // Over the whole window while dragging, so the moves keep coming
        // however fast the pointer leaves the bar.
        if let Some((from, start)) = drag() {
            div {
                class: "drag-veil",
                onmousemove: move |e: MouseEvent| {
                    // Released outside the window, where no mouseup reaches us.
                    if !e.held_buttons().contains(MouseButton::Primary) {
                        drag.set(None);
                        return;
                    }
                    let (_, page) = viewport();
                    let wanted = start + (from - e.client_coordinates().y);
                    *LOG_HEIGHT.write() =
                        wanted.clamp(MIN_LOG_HEIGHT, (page - 330.0).max(MIN_LOG_HEIGHT));
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            style: "flex:none;height:{height}px;max-height:calc(100vh - 330px);min-height:0;\
                    display:flex;flex-direction:column",
            LogStrip { revision }
        }
    }
}

// --- the rule editor ------------------------------------------------------------

/// The action a new rule starts with: anything but what already happens to
/// everything else, since a rule that repeats the fallback changes nothing.
fn suggested_action(fallback: Action) -> Action {
    match fallback {
        Action::Proxy => Action::Direct,
        Action::Direct | Action::Block | Action::Vpn => Action::Proxy,
    }
}

fn new_rule(action: Action, conditions: &str) {
    *DRAFT.write() = Some(Draft {
        at: None,
        name: String::new(),
        action,
        conditions: conditions.trim().to_owned(),
        problem: None,
        deleting: false,
    });
}

fn edit_rule(at: usize, rule: &bliss_proto::Rule) {
    *DRAFT.write() = Some(Draft {
        at: Some(at),
        name: rule.name.clone().unwrap_or_default(),
        action: rule.action,
        conditions: crate::rule_text::to_text(rule),
        problem: None,
        deleting: false,
    });
}

/// The shapes a condition takes, shown under the box they are typed into.
const CONDITION_HINTS: [(&str, &str); 8] = [
    ("youtube.com", "сайт и всё, что под ним"),
    ("full:a.example", "ровно это имя"),
    ("keyword:video", "слово где угодно в имени"),
    ("geosite:category-ru", "набор сайтов из geosite.dat"),
    ("geoip:ru", "адреса страны"),
    ("10.0.0.0/8", "адрес или подсеть"),
    ("port:443", "порт или диапазон 6881-6889"),
    (
        if cfg!(windows) {
            "process:telegram.exe"
        } else {
            "process:telegram-desktop"
        },
        "программа",
    ),
];

/// A label over a field in the editor.
const LABEL: &str = "font-size:12px;letter-spacing:.08em;text-transform:uppercase;\
                     color:var(--color-neutral-500)";

/// Adding or changing one rule, over the routing screen.
///
/// It closes by Save, Cancel, the cross, Esc or a click beside it — every way
/// out anyone might try. The screen before it offered an "add" that led
/// nowhere a rule could be written, and no obvious way back.
#[component]
fn RuleEditor(draft: Draft, vpn: bool) -> Element {
    let controller = use_context::<Controller>();
    let geoip = draft.conditions.to_ascii_lowercase().contains("geoip:");
    let heading = match draft.at {
        Some(at) => format!("Правило №{}", at + 1),
        None => "Новое правило".to_owned(),
    };
    let delete = {
        let controller = controller.clone();
        move || {
            let Some(current) = DRAFT() else { return };
            let Some(at) = current.at else { return };
            if current.deleting {
                controller.send(Command::DeleteRule(at));
                *DRAFT.write() = None;
            } else if let Some(open) = DRAFT.write().as_mut() {
                open.deleting = true;
            }
        }
    };
    let save = move || {
        let Some(current) = DRAFT() else { return };
        match crate::rule_text::parse(&current.name, current.action, &current.conditions) {
            Ok(rule) => {
                controller.send(Command::SaveRule {
                    at: current.at,
                    rule,
                });
                *DRAFT.write() = None;
            }
            Err(why) => {
                if let Some(open) = DRAFT.write().as_mut() {
                    open.problem = Some(why);
                }
            }
        }
    };
    // Cloned here rather than inside rsx: it builds handlers in its own order,
    // and a clone taken there can come after the button has moved the original.
    let save_on_key = save.clone();

    rsx! {
        div {
            class: "veil",
            // Ctrl+V leaves the window alone while this is open: see KEY_LISTENER.
            "data-modal": "true",
            onclick: move |_| *DRAFT.write() = None,
            div {
                class: "dialog-card",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                div { style: "flex:none;display:flex;align-items:center;gap:10px",
                    span {
                        style: "font-weight:700;font-size:20px;line-height:1.2;\
                                font-family:var(--font-heading)",
                        "{heading}"
                    }
                    div { style: "margin-left:auto",
                        IconButton {
                            title: "Закрыть — Esc",
                            enabled: true,
                            onclick: move |_| *DRAFT.write() = None,
                            icon: rsx! { crate::icons::Close { size: 16, color: MUTED } },
                        }
                    }
                }

                // Only this part scrolls, so the heading and the buttons stay in
                // view however far the notes below push the height.
                div { class: "dialog-body",
                    // Why a save was refused, first: after a Ctrl+Enter the
                    // reason must be where the eye already is, not scrolled away.
                    if let Some(problem) = &draft.problem {
                        span {
                            style: "flex:none;font-size:13px;line-height:1.45;color:#f2a7ad;\
                                    word-break:break-word",
                            "{problem}"
                        }
                    }

                    div { style: "flex:none;display:flex;flex-direction:column;gap:6px",
                        span { style: LABEL, "Название" }
                        input {
                            class: "field",
                            r#type: "text",
                            value: "{draft.name}",
                            placeholder: "можно не заполнять — назовётся по условиям",
                            autocomplete: "off",
                            spellcheck: "false",
                            style: "padding:8px 10px;border-radius:4px;border:none;\
                                    color:var(--color-text);font-size:14px",
                            oninput: move |e| {
                                if let Some(open) = DRAFT.write().as_mut() {
                                    open.name = e.value();
                                }
                            },
                        }
                    }

                    div { style: "flex:none;display:flex;flex-direction:column;gap:6px",
                        span { style: LABEL, "Что делать" }
                        // Wraps: with the VPN's choice there are four, and the
                        // dialog is narrower than four in the default window.
                        div { style: "display:flex;flex-wrap:wrap;gap:8px",
                            for action in actions(vpn, draft.action) {
                                div {
                                    key: "{action_key(action)}",
                                    class: choice_class("seg-choice", action == draft.action, action),
                                    onclick: move |_| {
                                        if let Some(open) = DRAFT.write().as_mut() {
                                            open.action = action;
                                        }
                                    },
                                    span { class: "menu-dot {action_key(action)}" }
                                    "{action_phrase(action)}"
                                }
                            }
                        }
                    }

                    div { style: "flex:none;display:flex;flex-direction:column;gap:6px",
                        span { style: LABEL, "Условия — по одному в строке, хватит любого" }
                        textarea {
                            class: "field",
                            value: "{draft.conditions}",
                            rows: "5",
                            spellcheck: "false",
                            placeholder: "geoip:ru\ngeosite:category-ru\nyoutube.com",
                            style: "min-height:112px;resize:vertical;padding:9px 10px;\
                                    border-radius:4px;border:none;color:var(--color-text);\
                                    font-size:14px;line-height:1.55;\
                                    font-family:ui-monospace,Consolas,monospace",
                            onmounted: move |e: MountedEvent| async move {
                                let _ = e.data().set_focus(true).await;
                            },
                            oninput: move |e| {
                                if let Some(open) = DRAFT.write().as_mut() {
                                    open.conditions = e.value();
                                    open.problem = None;
                                }
                            },
                            onkeydown: move |e: KeyboardEvent| {
                                if e.key() == Key::Enter && e.modifiers().ctrl() {
                                    e.prevent_default();
                                    save_on_key();
                                }
                            },
                        }
                    }

                    if geoip {
                        span {
                            style: "flex:none;font-size:12.5px;line-height:1.5;color:#cbbaf4;\
                                    padding:9px 11px;border-radius:4px;\
                                    background-color:rgba(203,186,244,.07);\
                                    box-shadow:inset 0 0 0 1px rgba(203,186,244,.18)",
                            "geoip сравнивает адреса, а не имена сайтов. Браузер через прокси \
                             называет сайт по имени, так что это правило поймает только \
                             программы, которые подключаются сразу по адресу. Сайты страны \
                             ловит geosite — например, geosite:category-ru."
                        }
                    }

                    div {
                        style: "flex:none;display:grid;grid-template-columns:auto 1fr;gap:2px 14px;\
                                font-size:12.5px;line-height:1.5",
                        for (example, meaning) in CONDITION_HINTS {
                            div { key: "{example}", style: "display:contents",
                                span {
                                    style: "font-family:ui-monospace,Consolas,monospace;\
                                            color:var(--color-neutral-300)",
                                    "{example}"
                                }
                                span { style: "color:var(--color-neutral-600)", "{meaning}" }
                            }
                        }
                    }
                }

                div { style: "flex:none;display:flex;align-items:center;gap:8px;padding-top:2px",
                    if draft.at.is_some() {
                        Button {
                            label: if draft.deleting { "Точно удалить?" } else { "Удалить" },
                            enabled: true,
                            onclick: move |_| delete(),
                            icon: rsx! { crate::icons::Trash { size: 15, color: BAD } },
                        }
                    }
                    span {
                        style: "margin-left:auto;font-size:12px;color:var(--color-neutral-700);\
                                white-space:nowrap",
                        "Ctrl+Enter — сохранить"
                    }
                    Button {
                        label: "Отмена",
                        enabled: true,
                        onclick: move |_| *DRAFT.write() = None,
                        icon: rsx! { crate::icons::Close { size: 15, color: MUTED } },
                    }
                    Button {
                        label: "Сохранить",
                        enabled: true,
                        onclick: move |_| save(),
                        icon: rsx! { crate::icons::Check { size: 15, color: ACCENT } },
                    }
                }
            }
        }
    }
}

// --- narrowing the table to one rule ------------------------------------------

/// The rules to narrow the connections table to, each with how many of the
/// rows it decided: the routing table's rules in their order, then
/// "по умолчанию", then any name only the rows still carry — a rule deleted or
/// renamed since it decided them.
fn rule_choices(
    routing: &crate::engine::RoutingView,
    rows: &[bliss_core::Snapshot],
) -> Vec<(String, usize)> {
    /// What the router calls a decision no rule made.
    const FALLBACK: &str = "по умолчанию";
    let mut names: Vec<String> = Vec::new();
    let named = routing
        .rules
        .iter()
        .map(|rule| rule.name.as_str())
        .chain(std::iter::once(FALLBACK))
        .chain(rows.iter().map(|row| row.rule.as_str()));
    for name in named {
        if !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
    }
    names
        .into_iter()
        .map(|name| {
            let count = rows.iter().filter(|row| row.rule == name).count();
            (name, count)
        })
        .collect()
}

/// Narrows the connections table to what one rule decided.
///
/// A list rather than words in the search box: a rule's name is not shown in
/// the table, so typing one matched rows for a reason nobody could see.
#[component]
fn RuleFilter(choices: Vec<(String, usize)>, total: usize) -> Element {
    let chosen = TABLE_RULE();
    let open = RULE_MENU();
    let label = match &chosen {
        Some(rule) => elide(rule, 22),
        None => "все правила".to_owned(),
    };
    rsx! {
        div { style: "position:relative;flex:none",
            div {
                class: if chosen.is_some() { "act filter on" } else { "act filter" },
                style: "display:inline-flex;align-items:center;gap:7px;padding:6px 10px 6px 11px;\
                        border-radius:4px;font-size:13.5px;white-space:nowrap",
                title: "Показать соединения одного правила",
                onclick: move |_| {
                    let now = RULE_MENU();
                    *RULE_MENU.write() = !now;
                },
                crate::icons::Split { size: 14, color: if chosen.is_some() { ACCENT } else { MUTED } }
                "{label}"
                crate::icons::ChevronDown { size: 14, color: MUTED }
            }
            if open {
                div { class: "menu-veil", onclick: move |_| *RULE_MENU.write() = false }
                div { class: "dropdown",
                    div {
                        class: if chosen.is_none() { "menu-item chosen" } else { "menu-item" },
                        onclick: move |_| {
                            *TABLE_RULE.write() = None;
                            *RULE_MENU.write() = false;
                        },
                        span { style: "flex:1", "Все правила" }
                        span { class: "menu-count", "{total}" }
                    }
                    div { class: "menu-sep" }
                    for (name, count) in choices {
                        div {
                            key: "{name}",
                            class: if chosen.as_deref() == Some(name.as_str()) {
                                "menu-item chosen"
                            } else if count == 0 {
                                "menu-item quiet"
                            } else {
                                "menu-item"
                            },
                            onclick: {
                                let name = name.clone();
                                move |_| {
                                    *TABLE_RULE.write() = Some(name.clone());
                                    *RULE_MENU.write() = false;
                                }
                            },
                            span { style: "flex:1;min-width:0;overflow:hidden;white-space:nowrap",
                                "{elide(&name, 30)}"
                            }
                            span { class: "menu-count", "{count}" }
                        }
                    }
                }
            }
        }
    }
}

// --- the server list ------------------------------------------------------------

/// The server list's columns: name, address, protection, ping, and the bin.
/// Name and address never grow past their share, for the reason given at
/// `CONNECTION_COLUMNS`. This is the shape before the header has measured the
/// list; after that `ServerColumns` gives it in pixels.
const SERVER_COLUMNS: &str =
    "grid-template-columns:minmax(0,1.2fr) minmax(0,1.4fr) 136px 78px 30px;gap:12px";
const SERVER_KIND: f64 = 136.0;
/// Ping, the bin, and the four gaps between the five columns.
const SERVER_FIXED: f64 = 78.0 + 30.0 + 4.0 * 12.0;
/// The row's padding on either side, as the header has it.
const SERVER_PAD: f64 = 10.0;
/// Narrower than this, a name or an address is no longer worth a column.
const MIN_TEXT_COLUMN: f64 = 80.0;

/// The server list's columns in pixels, once the list's width is known.
///
/// The name and the protocol can be dragged wider or narrower — VPN providers
/// name their servers at length, and a name cut at 22 characters left servers
/// that could not be told apart. The address takes what is left, so the row
/// always fits the list, whatever the window does after a drag.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ServerColumns {
    name: f64,
    addr: f64,
    kind: f64,
}

impl ServerColumns {
    /// `width` is the list's content width; `name` and `kind` what someone
    /// dragged them to, if they did.
    fn new(width: f64, name: Option<f64>, kind: Option<f64>) -> Option<Self> {
        let text = width - SERVER_PAD * 2.0 - SERVER_FIXED;
        if text < MIN_TEXT_COLUMN * 2.0 + 70.0 {
            return None;
        }
        let kind = kind
            .unwrap_or(SERVER_KIND)
            .clamp(70.0, (text - MIN_TEXT_COLUMN * 2.0).min(320.0))
            .round();
        let rest = text - kind;
        // The default share is the one the fractions above give.
        let name = name
            .unwrap_or(rest * 1.2 / 2.6)
            .clamp(MIN_TEXT_COLUMN, rest - MIN_TEXT_COLUMN)
            .round();
        Some(Self {
            name,
            addr: rest - name,
            kind,
        })
    }

    /// The address is `1fr` rather than its pixels, so that a grouped row,
    /// indented by 20px, loses them from the address and not off its end.
    fn css(&self) -> String {
        format!(
            "grid-template-columns:{}px minmax(0,1fr) {}px 78px 30px;gap:12px",
            self.name, self.kind
        )
    }
}

/// How many characters of text `px` wide fit, at `size` pixels a letter.
///
/// The renderer subset has no `text-overflow` (rule 5), so text is cut in
/// code, and a guess at the letter's width is all there is: Comfortaa's
/// letters average about half an em, wide capitals and Cyrillic more, so 0.6
/// leaves room for a name of capitals rather than cutting it mid-letter.
fn letters_in(px: f64, size: f64) -> usize {
    ((px / (size * 0.6)).floor() as usize).max(4)
}

/// Which border between the server list's columns a drag moves.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ColumnBorder {
    /// Between the name and the address: moves the name's right edge.
    Name,
    /// Between the address and the protocol: moves the protocol's left edge.
    Kind,
}

/// A run of the server list: the servers added by hand, or one subscription's.
#[derive(Clone, Debug, PartialEq)]
struct Section {
    /// The subscription's id and name; `None` for servers added by hand.
    group: Option<(String, String)>,
    /// Places in `frame.servers`, which is what a click selects by.
    rows: Vec<usize>,
    open: bool,
    has_current: bool,
}

/// The server list in sections: servers added by hand first, then each
/// subscription's under its name, in the order they first appear.
///
/// A subscription can bring hundreds of servers, so its group starts closed,
/// unless the server in use is in it; a group someone opened or closed stays
/// the way they left it (`open`).
fn server_sections(
    servers: &[crate::engine::ServerRow],
    open: &std::collections::HashMap<String, bool>,
) -> Vec<Section> {
    let mine: Vec<usize> = servers
        .iter()
        .enumerate()
        .filter(|(_, server)| server.group_id.is_none())
        .map(|(i, _)| i)
        .collect();
    let mut sections = Vec::new();
    if !mine.is_empty() {
        sections.push(Section {
            group: None,
            rows: mine,
            open: true,
            has_current: false,
        });
    }
    for (i, server) in servers.iter().enumerate() {
        let Some(id) = &server.group_id else {
            continue;
        };
        let existing = sections
            .iter_mut()
            .find(|section| section.group.as_ref().is_some_and(|(known, _)| known == id));
        match existing {
            Some(section) => {
                section.rows.push(i);
                section.has_current |= server.current;
            }
            None => sections.push(Section {
                group: Some((id.clone(), server.group.clone().unwrap_or_default())),
                rows: vec![i],
                open: false,
                has_current: server.current,
            }),
        }
    }
    for section in &mut sections {
        if let Some((id, _)) = &section.group {
            section.open = open.get(id).copied().unwrap_or(section.has_current);
        }
    }
    sections
}

/// One server: a click switches to it, the bin deletes it after a second click.
#[component]
fn ServerLine(
    index: usize,
    server: crate::engine::ServerRow,
    grouped: bool,
    mut armed: Signal<Option<usize>>,
) -> Element {
    let controller = use_context::<Controller>();
    let select = controller.clone();
    let arming = armed() == Some(index);
    let columns = ServerColumns::new(
        SERVER_LIST_WIDTH(),
        SERVER_NAME_WIDTH(),
        SERVER_KIND_WIDTH(),
    );
    let grid = columns.map_or_else(|| SERVER_COLUMNS.to_owned(), |c| c.css());
    // Cut to the columns' measured widths; 22 and 26 letters before the list
    // has been measured, as the fractions drew it in the default window.
    let (name_letters, addr_letters) = columns.map_or((22, 26), |c| {
        let indent = if grouped { 20.0 } else { 0.0 };
        (letters_in(c.name, 14.5), letters_in(c.addr - indent, 13.5))
    });
    let name = elide(&server.tag, name_letters);
    let tag = server.tag.clone();
    // Indented under its subscription's name. Always present, so a changing
    // value is all the renderer ever sees here (the note at the top).
    let left = if grouped { 30 } else { 10 };
    let title = if arming {
        "Нажмите ещё раз, чтобы удалить"
    } else if server.group.is_some() {
        "Удалить — вернётся при обновлении подписки"
    } else {
        "Удалить"
    };
    rsx! {
        div {
            // Marked by class, not by inline style: see the note in NavItem. A
            // property that vanishes from a style string is restored by the
            // renderer, so the previous server would keep its marker.
            class: if server.current { "srv current" } else { "srv" },
            style: "display:grid;{grid};align-items:center;padding:8px 10px 8px {left}px;\
                    border-radius:4px",
            onclick: move |_| select.send(Command::Select(index)),
            onmouseleave: move |_| {
                if armed() == Some(index) {
                    armed.set(None);
                }
            },
            span {
                style: "font-size:14.5px;min-width:0;overflow:hidden;white-space:nowrap",
                title: "{server.tag}",
                "{name}"
            }
            span {
                style: "min-width:0;overflow:hidden;white-space:nowrap;font-size:13.5px;\
                        color:var(--color-neutral-400);font-family:ui-monospace,Consolas,monospace",
                title: "{server.addr}",
                "{elide(&server.addr, addr_letters)}"
            }
            span {
                style: "overflow:hidden;white-space:nowrap;font-size:13px;color:var(--color-neutral-500)",
                title: "{server.detail}",
                "{server.kind}"
            }
            span {
                style: "text-align:right;font-size:13.5px;\
                        font-variant-numeric:tabular-nums;color:{ping_tone(server.ping)}",
                title: if server.ping == Ping::Udp {
                    "Сервер работает по UDP — рукопожатие TCP до него ничего не измерит"
                } else {
                    ""
                },
                "{ping_text(server.ping)}"
            }
            div { class: "srv-tools",
                IconButton {
                    title,
                    enabled: true,
                    danger: true,
                    onclick: move |_| {
                        if armed() == Some(index) {
                            armed.set(None);
                            controller.send(Command::DeleteServer { index, tag: tag.clone() });
                        } else {
                            armed.set(Some(index));
                        }
                    },
                    icon: rsx! {
                        crate::icons::Trash { size: 14, color: if arming { BAD } else { MUTED } }
                    },
                }
            }
        }
    }
}

// --- screen: subscriptions ----------------------------------------------------

/// The provider lists bliss fetches its servers from.
#[component]
fn Subscriptions(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let servers: usize = frame.subscriptions.iter().map(|s| s.servers).sum();
    let all_busy = frame.subscriptions.iter().all(|s| s.fetching);
    // Left open, the editor would be waiting on the next visit.
    use_drop(|| *SUB_DRAFT.write() = None);

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px 18px 12px",
            div { style: "{SCREEN_HEAD}",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Подписки" }
                span {
                    style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;\
                            overflow:hidden;white-space:nowrap",
                    "{plural(frame.subscriptions.len() as u64, \"подписка\", \"подписки\", \"подписок\")} · \
                     {plural(servers as u64, \"сервер\", \"сервера\", \"серверов\")}"
                }
                div { style: "margin-left:auto;flex:none;align-self:center",
                    Button {
                        label: "Обновить все",
                        enabled: !frame.subscriptions.is_empty() && !all_busy,
                        onclick: move |_| controller.send(Command::Refresh(None)),
                        icon: rsx! { crate::icons::Refresh { size: 15, color: ACCENT } },
                    }
                }
            }

            // The form scrolls with the cards, in one column, so it is exactly
            // as wide as they are. Above the scrolling list it was wider by the
            // list's scrollbar, and no card's edge lined up with it.
            div {
                style: "{SCROLL_COLUMN};\
                        display:flex;flex-direction:column;gap:12px",
                div {
                    style: "flex:none;display:flex;flex-direction:column;gap:10px;padding:16px;\
                            background-color:var(--color-surface);border-radius:8px;\
                            box-shadow:var(--shadow-sm)",
                    span { style: "font:700 16px/1 var(--font-heading)", "Добавить подписку" }
                    span { style: "font-size:13px;line-height:1.5;color:var(--color-neutral-500)",
                        "Адрес списка серверов от провайдера. bliss загрузит список — через туннель, \
                         если он поднят, — и будет обновлять его раз в 12 часов. Ctrl+V в любом месте \
                         окна тоже добавляет."
                    }
                    ImportForm { frame: frame.clone(), placeholder: "https://…", wide: true }
                }

                div { style: "flex:none;display:flex;flex-direction:column;gap:9px",
                    if frame.subscriptions.is_empty() {
                        span {
                            style: "padding:10px 2px;font-size:14px;line-height:1.5;\
                                    color:var(--color-neutral-600)",
                            "Подписок пока нет. Серверы из подписки появятся в списке серверов \
                             и будут обновляться сами."
                        }
                    }
                    for row in frame.subscriptions.iter() {
                        SubscriptionCard { key: "{row.id}", row: row.clone() }
                    }
                }
            }
        }
        if let Some(draft) = SUB_DRAFT() {
            SubscriptionEditor { draft }
        }
    }
}

#[component]
fn SubscriptionCard(row: crate::engine::SubscriptionRow) -> Element {
    let controller = use_context::<Controller>();
    // "Удалить" clicked once; the second click deletes.
    let mut armed = use_signal(|| false);
    let now = jiff::Timestamp::now().as_second();
    let (status, tone) = if row.fetching {
        ("загружаю список…".to_owned(), "var(--color-accent-2-300)")
    } else if let Some(problem) = &row.problem {
        (problem.clone(), "#f2a7ad")
    } else {
        match row.updated {
            Some(at) => (
                format!("обновлена {}", ago(at, now)),
                "var(--color-neutral-500)",
            ),
            None => ("ещё не загружена".to_owned(), "var(--color-neutral-500)"),
        }
    };
    let usage = row.usage.map(|usage| {
        let used = usage.upload.saturating_add(usage.download);
        let amount = if usage.total > 0 {
            format!("{} из {}", bytes(used), bytes(usage.total))
        } else {
            format!("израсходовано {}", bytes(used))
        };
        let until = usage
            .expire
            .and_then(date)
            .map(|day| format!(" · до {day}"))
            .unwrap_or_default();
        let share = if usage.total > 0 {
            (used as f64 / usage.total as f64).clamp(0.0, 1.0) * 100.0
        } else {
            0.0
        };
        (format!("{amount}{until}"), share, usage.total > 0)
    });
    let refresh = {
        let controller = controller.clone();
        let id = row.id.clone();
        move |_| controller.send(Command::Refresh(Some(id.clone())))
    };
    let edit = {
        let draft = SubDraft {
            id: row.id.clone(),
            name: row.name.clone(),
            url: String::new(),
            shown_url: row.shown_url.clone(),
            problem: None,
        };
        move |_| *SUB_DRAFT.write() = Some(draft.clone())
    };
    let copy = {
        let controller = controller.clone();
        let id = row.id.clone();
        move |_| controller.send(Command::CopySubscription(id.clone()))
    };
    let id = row.id.clone();

    rsx! {
        div {
            // The form's padding above it, so their contents start at one edge.
            style: "flex:none;display:flex;align-items:flex-start;gap:13px;padding:16px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            onmouseleave: move |_| armed.set(false),
            div {
                style: "width:36px;height:36px;flex:none;display:grid;place-items:center;\
                        border-radius:8px;background-color:rgba(238,130,196,.10)",
                crate::icons::Rss { size: 18, color: ACCENT }
            }
            div { style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:4px",
                span { style: "font-size:15px;font-weight:700", "{elide(&row.name, 44)}" }
                span {
                    style: "font-size:12.5px;color:var(--color-neutral-600);\
                            font-family:ui-monospace,Consolas,monospace",
                    "{row.shown_url}"
                }
                span { style: "font-size:13px;line-height:1.45;word-break:break-word",
                    span { style: "color:var(--color-neutral-400)",
                        "{plural(row.servers as u64, \"сервер\", \"сервера\", \"серверов\")} · "
                    }
                    span { style: "color:{tone}", "{status}" }
                }
                if let Some((text, share, limited)) = usage {
                    div { style: "display:flex;flex-direction:column;gap:5px;max-width:420px;padding-top:3px",
                        if limited {
                            div {
                                style: "height:5px;border-radius:3px;overflow:hidden;\
                                        background-color:rgba(239,232,245,.08)",
                                div {
                                    style: "height:100%;width:{share}%;border-radius:3px;\
                                            background-color:{usage_tone(share)}",
                                }
                            }
                        }
                        span {
                            style: "font-size:12.5px;color:var(--color-neutral-500);\
                                    font-variant-numeric:tabular-nums",
                            "{text}"
                        }
                    }
                }
            }
            div { style: "flex:none;display:flex;align-items:center;gap:6px",
                IconButton {
                    title: "Изменить название или адрес",
                    enabled: true,
                    onclick: edit,
                    icon: rsx! { crate::icons::Pencil { size: 15, color: MUTED } },
                }
                IconButton {
                    title: "Скопировать адрес подписки",
                    enabled: true,
                    onclick: copy,
                    icon: rsx! { crate::icons::Copy { size: 15, color: MUTED } },
                }
                Button {
                    label: if row.fetching { "Загружаю…" } else { "Обновить" },
                    enabled: !row.fetching,
                    onclick: refresh,
                    icon: rsx! { crate::icons::Refresh { size: 15, color: ACCENT } },
                }
                Button {
                    label: if armed() { "Точно удалить?" } else { "Удалить" },
                    enabled: true,
                    onclick: move |_| {
                        if armed() {
                            armed.set(false);
                            controller.send(Command::DeleteSubscription(id.clone()));
                        } else {
                            armed.set(true);
                        }
                    },
                    icon: rsx! { crate::icons::Trash { size: 15, color: BAD } },
                }
            }
        }
    }
}

/// Changing a subscription's name or its address, over the subscriptions
/// screen. It closes the ways the rule editor does: Save, Cancel, the cross,
/// Esc or a click beside it.
///
/// The address box starts empty. The window shows a subscription's host and
/// nothing after it, where its token is; a new address replaces the old, and
/// the old one can be had whole from «Скопировать адрес».
#[component]
fn SubscriptionEditor(draft: SubDraft) -> Element {
    let controller = use_context::<Controller>();
    let save = {
        let controller = controller.clone();
        move || {
            let Some(current) = SUB_DRAFT() else { return };
            let url = current.url.trim().to_owned();
            if !url.is_empty() && crate::profile_file::subscription_address(&url).is_none() {
                if let Some(open) = SUB_DRAFT.write().as_mut() {
                    open.problem = Some(
                        "это не адрес подписки: нужен один адрес, начинающийся с http:// \
                         или https://"
                            .to_owned(),
                    );
                }
                return;
            }
            controller.send(Command::EditSubscription {
                id: current.id,
                name: current.name,
                url: (!url.is_empty()).then_some(Link(url)),
            });
            *SUB_DRAFT.write() = None;
        }
    };
    // Cloned here rather than inside rsx, for the reason RuleEditor gives.
    let save_on_name = save.clone();
    let save_on_url = save.clone();
    let copy = {
        let id = draft.id.clone();
        move |_| controller.send(Command::CopySubscription(id.clone()))
    };
    let address_hint = format!("{} — вставьте новый, чтобы заменить", draft.shown_url);

    rsx! {
        div {
            class: "veil",
            // Ctrl+V leaves the window alone while this is open: see KEY_LISTENER.
            "data-modal": "true",
            onclick: move |_| *SUB_DRAFT.write() = None,
            div {
                class: "dialog-card",
                onclick: move |e: MouseEvent| e.stop_propagation(),
                div { style: "flex:none;display:flex;align-items:center;gap:10px",
                    span {
                        style: "font-weight:700;font-size:20px;line-height:1.2;\
                                font-family:var(--font-heading)",
                        "Подписка"
                    }
                    div { style: "margin-left:auto",
                        IconButton {
                            title: "Закрыть — Esc",
                            enabled: true,
                            onclick: move |_| *SUB_DRAFT.write() = None,
                            icon: rsx! { crate::icons::Close { size: 16, color: MUTED } },
                        }
                    }
                }

                div { class: "dialog-body",
                    if let Some(problem) = &draft.problem {
                        span {
                            style: "flex:none;font-size:13px;line-height:1.45;color:#f2a7ad;\
                                    word-break:break-word",
                            "{problem}"
                        }
                    }

                    div { style: "flex:none;display:flex;flex-direction:column;gap:6px",
                        span { style: LABEL, "Название" }
                        input {
                            class: "field",
                            r#type: "text",
                            value: "{draft.name}",
                            placeholder: "пусто — как назовёт провайдер",
                            autocomplete: "off",
                            spellcheck: "false",
                            style: "padding:8px 10px;border-radius:4px;border:none;\
                                    color:var(--color-text);font-size:14px",
                            onmounted: move |e: MountedEvent| async move {
                                let _ = e.data().set_focus(true).await;
                            },
                            oninput: move |e| {
                                if let Some(open) = SUB_DRAFT.write().as_mut() {
                                    open.name = e.value();
                                }
                            },
                            onkeydown: move |e: KeyboardEvent| {
                                if e.key() == Key::Enter {
                                    e.prevent_default();
                                    save_on_name();
                                }
                            },
                        }
                        span {
                            style: "font-size:12.5px;line-height:1.5;color:var(--color-neutral-600)",
                            "Своё название остаётся и после обновлений. Без него подписку называет \
                             провайдер, а пока он не назвал — адрес."
                        }
                    }

                    div { style: "flex:none;display:flex;flex-direction:column;gap:6px",
                        span { style: LABEL, "Адрес" }
                        input {
                            class: "field",
                            r#type: "text",
                            value: "{draft.url}",
                            placeholder: "{address_hint}",
                            autocomplete: "off",
                            spellcheck: "false",
                            style: "padding:8px 10px;border-radius:4px;border:none;\
                                    color:var(--color-text);font-size:14px;\
                                    font-family:ui-monospace,Consolas,monospace",
                            oninput: move |e| {
                                if let Some(open) = SUB_DRAFT.write().as_mut() {
                                    open.url = e.value();
                                    open.problem = None;
                                }
                            },
                            onkeydown: move |e: KeyboardEvent| {
                                if e.key() == Key::Enter {
                                    e.prevent_default();
                                    save_on_url();
                                }
                            },
                        }
                        span {
                            style: "font-size:12.5px;line-height:1.5;color:var(--color-neutral-600)",
                            "Целиком адрес не показывается: в нём ваш личный токен. Пустое поле \
                             оставляет прежний, а с новым список загрузится сразу."
                        }
                        div { style: "display:flex;padding-top:2px",
                            Button {
                                label: "Скопировать адрес",
                                enabled: true,
                                onclick: copy,
                                icon: rsx! { crate::icons::Copy { size: 15, color: ACCENT } },
                            }
                        }
                    }
                }

                div { style: "flex:none;display:flex;align-items:center;gap:8px;padding-top:2px",
                    span {
                        style: "margin-left:auto;font-size:12px;color:var(--color-neutral-700);\
                                white-space:nowrap",
                        "Enter — сохранить"
                    }
                    Button {
                        label: "Отмена",
                        enabled: true,
                        onclick: move |_| *SUB_DRAFT.write() = None,
                        icon: rsx! { crate::icons::Close { size: 15, color: MUTED } },
                    }
                    Button {
                        label: "Сохранить",
                        enabled: true,
                        onclick: move |_| save(),
                        icon: rsx! { crate::icons::Check { size: 15, color: ACCENT } },
                    }
                }
            }
        }
    }
}

/// Violet while there is plenty left, the failure colour near the end.
fn usage_tone(percent: f64) -> &'static str {
    if percent >= 90.0 { BAD } else { "#a98ce8" }
}

/// How long ago, in the words a person would use.
fn ago(then: i64, now: i64) -> String {
    let seconds = now.saturating_sub(then).max(0).unsigned_abs();
    match seconds {
        0..60 => "только что".to_owned(),
        60..3600 => format!(
            "{} назад",
            plural(seconds / 60, "минуту", "минуты", "минут")
        ),
        3600..86_400 => format!("{} назад", plural(seconds / 3600, "час", "часа", "часов")),
        _ => format!("{} назад", plural(seconds / 86_400, "день", "дня", "дней")),
    }
}

/// A day, the way a Russian date is written.
fn date(unix: i64) -> Option<String> {
    let zoned = jiff::Timestamp::from_second(unix)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system());
    Some(zoned.strftime("%d.%m.%Y").to_string())
}

// --- screen: settings -----------------------------------------------------------

/// The choices about bliss itself. For now: whether Windows starts it.
#[component]
fn Settings(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    // Read again whenever the screen opens: Task Manager can switch the entry
    // off, and another copy of bliss take it over, while this one runs.
    use_hook({
        let controller = controller.clone();
        move || controller.send(Command::ReadStartup)
    });
    let startup = frame.startup.clone();
    let (on, minimized) = (startup.on, startup.minimized);
    const NOTE: &str = "padding:2px 0 4px 29px;font-size:12.5px;line-height:1.45;color:#cbbaf4";

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px 18px 12px",
            div { style: "{SCREEN_HEAD}",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Настройки" }
            }

            div {
                style: "{SCROLL_COLUMN};\
                        display:flex;flex-direction:column;gap:12px",
                div {
                    style: "flex:none;display:flex;flex-direction:column;gap:4px;padding:16px;\
                            background-color:var(--color-surface);border-radius:8px;\
                            box-shadow:var(--shadow-sm)",
                    span { style: "font:700 16px/1 var(--font-heading);padding:0 0 8px", "Запуск" }
                    CheckRow {
                        checked: on,
                        enabled: true,
                        title: if cfg!(windows) {
                            "Запускать вместе с Windows"
                        } else {
                            "Запускать при входе в систему"
                        },
                        hint: "bliss откроется, когда вы войдёте в систему, и сразу поднимет туннель.",
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Startup { on: !on, minimized })
                        },
                    }
                    // Only for an entry that exists: the flag is part of it.
                    CheckRow {
                        checked: on && minimized,
                        enabled: on,
                        title: "В трее",
                        hint: if cfg!(windows) {
                            "Окно не появится: bliss будет ждать значком у часов."
                        } else {
                            "Окно не появится: bliss будет ждать значком в трее, а где трея \
                             нет, откроется свёрнутым."
                        },
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Startup { on: true, minimized: !minimized })
                        },
                    }
                    // Linux: an entry in the application menu, which is also
                    // where a Wayland desktop takes the window's icon from.
                    if let Some(listed) = startup.menu {
                        CheckRow {
                            checked: listed,
                            enabled: true,
                            title: "Значок в меню приложений",
                            hint: "bliss появится среди программ, и у его окна будет свой значок.",
                            onclick: {
                                let controller = controller.clone();
                                move |_| controller.send(Command::MenuEntry(!listed))
                            },
                        }
                    }
                    if startup.switched_off {
                        span { style: NOTE,
                            if cfg!(windows) {
                                "Автозапуск выключен в Диспетчере задач. Поставьте галочку, и \
                                 bliss включит его снова."
                            } else {
                                "Автозапуск выключен в настройках рабочего стола. Поставьте \
                                 галочку, и bliss включит его снова."
                            }
                        }
                    }
                    if let Some(path) = startup.elsewhere.clone() {
                        div { style: "display:flex;align-items:center;flex-wrap:wrap;gap:8px 12px;\
                                      padding:2px 0 4px 29px",
                            span {
                                style: "min-width:0;font-size:12.5px;line-height:1.45;color:#cbbaf4;\
                                        word-break:break-all",
                                "При входе в систему запускается другая копия: {path}"
                            }
                            Button {
                                label: "Запускать эту",
                                enabled: true,
                                onclick: {
                                    let controller = controller.clone();
                                    move |_| controller.send(Command::Startup { on: true, minimized })
                                },
                                icon: rsx! { crate::icons::Check { size: 15, color: ACCENT } },
                            }
                        }
                    }
                    if let Some(problem) = startup.problem.clone() {
                        span {
                            style: "padding:2px 0 4px 29px;font-size:12.5px;line-height:1.45;color:#f2a7ad",
                            "{problem}"
                        }
                    }
                }
                VpnCard { vpn: frame.vpn.clone() }
            }
        }
    }
}

/// A choice that is on or off: a box with a tick, a title, and a line on what
/// it does. How each state looks is `.check-row` in app.css — classes, not
/// inline style (the note at the top of this file).
#[component]
fn CheckRow(
    checked: bool,
    enabled: bool,
    title: String,
    hint: String,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let class = match (enabled, checked) {
        (false, _) => "check-row disabled",
        (true, true) => "check-row on",
        (true, false) => "check-row",
    };
    rsx! {
        div {
            class: "{class}",
            style: "display:flex;align-items:flex-start;gap:11px;margin:0 -10px;padding:9px 10px;\
                    border-radius:4px",
            onclick: move |e| if enabled { onclick.call(e) },
            div { class: "check-box",
                if checked {
                    // The window's ground colour, on the accent the box fills with.
                    crate::icons::Check { size: 13, color: "#140f1d" }
                }
            }
            div { style: "display:flex;flex-direction:column;gap:3px;min-width:0",
                span { style: "font-size:14.5px", "{title}" }
                span {
                    style: "font-size:12.5px;line-height:1.45;color:var(--color-neutral-600)",
                    "{hint}"
                }
            }
        }
    }
}
