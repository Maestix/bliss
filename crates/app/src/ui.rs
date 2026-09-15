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

use crate::engine::{Command, Controller, Frame, Link, Ping, Reveal, State, bytes, rate};
use crate::profile_file::Subject;
use bliss_proto::{Action, ProxyMode};

/// The window's palette, type and radii — a calm neon; see theme.css.
const THEME: &str = include_str!("../assets/theme.css");
const APP_CSS: &str = include_str!("../assets/app.css");
/// The status card's night scene; see `.status-card` in app.css.
const SCENE: &[u8] = include_bytes!("../assets/status-card.jpg");
/// How app.css names the scene, for `STYLES` to replace.
const SCENE_URL: &str = "url(status-card.jpg)";
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

/// app.css with the scene inlined. Built once: encoding a picture on every
/// render would be work for nothing.
static STYLES: LazyLock<String> =
    LazyLock::new(|| APP_CSS.replace(SCENE_URL, &data_url("image/jpeg", SCENE)));

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
/// Soft pink, `--color-accent` in theme.css: what is on, chosen or clickable.
const ACCENT: &str = "#ee82c4";
/// Neutral-400: window controls and anything else that should recede.
const MUTED: &str = "#b5a9c4";
/// `--color-good`: a quick server, a rule that sends traffic direct.
const GOOD: &str = "#72cf9e";
/// `--color-bad`: a failure, a server that did not answer, a blocking rule.
const BAD: &str = "#e56b6f";

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

/// The log pane's height on the logs screen, in CSS pixels. Global, because
/// the screen is built afresh on every visit and the height should outlive it.
static LOG_HEIGHT: GlobalSignal<f64> = Signal::global(|| DEFAULT_LOG_HEIGHT);
const DEFAULT_LOG_HEIGHT: f64 = 230.0;
const MIN_LOG_HEIGHT: f64 = 110.0;
/// What the connections table is narrowed to, and in which order.
static TABLE_QUERY: GlobalSignal<String> = Signal::global(String::new);
static TABLE_SORT: GlobalSignal<Sort> = Signal::global(|| Sort::Newest);
/// The right-click menu over a connection, while it is open. Global so Esc,
/// heard at the top of the window, can close it.
static ROW_MENU: GlobalSignal<Option<RowMenu>> = Signal::global(|| None);
/// The rule editor, while it is open. Global for the same reason.
static DRAFT: GlobalSignal<Option<Draft>> = Signal::global(|| None);
/// The rule the connections table is narrowed to; `None` for every rule.
static TABLE_RULE: GlobalSignal<Option<String>> = Signal::global(|| None);
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

pub fn launch(controller: Controller) {
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            Config::new()
                .with_window(
                    WindowBuilder::new()
                        .with_title("bliss")
                        // The design draws its own chrome, so Windows must not draw
                        // any. Everything the frame used to do — drag, minimise,
                        // maximise, close — is re-implemented in TitleBar below.
                        .with_decorations(false)
                        .with_inner_size(LogicalSize::new(1040.0, 660.0))
                        .with_min_inner_size(LogicalSize::new(880.0, 560.0))
                        // Dioxus puts its own logo on any window built without one.
                        .with_window_icon(app_icon(None)),
                )
                .with_on_window(|window, _| set_icons(&window, window.scale_factor())),
        )
        .with_context(controller)
        .launch(App);
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

#[cfg(not(windows))]
fn app_icon(_side: Option<u32>) -> Option<Icon> {
    None
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
}

impl Screen {
    fn key(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Traffic => "traffic",
            Self::Subscriptions => "subscriptions",
            Self::Routing => "routing",
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

    // The engine pushes; we never poll. A watch notification needs no timer, so
    // this works regardless of which executor is under the renderer.
    //
    // The controller is captured from the render scope rather than fetched
    // inside the closure: use_context is a hook, and hooks may only be called
    // while rendering.
    let watcher = controller.clone();
    use_future(move || {
        let mut controller = watcher.clone();
        async move {
            loop {
                controller.changed().await;
                frame.set(controller.frame());
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
                        text: Link(crate::clipboard::paste().unwrap_or_default()),
                        from_form: false,
                    }),
                    "escape" => {
                        *ROW_MENU.write() = None;
                        *RULE_MENU.write() = false;
                        *DRAFT.write() = None;
                    }
                    _ => {}
                }
            }
        }
    });

    let frame = frame();

    rsx! {
        Styles {}
        WindowIcons {}
        WindowZoom {}
        ToastLine { toast: frame.toast.clone() }
        // The title bar spans the whole window, as in the reference, with the
        // sidebar beginning underneath it.
        div {
            style: "width:100vw;height:100vh;display:flex;flex-direction:column;\
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
            // Settings exist in the design but not yet in the program. Shown so
            // the shape is right, dimmed and inert so nothing claims to work
            // that does not.
            NavItem {
                label: "Настройки",
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Gear { size: 18, color: nav_tint(false, false) } },
            }

            div {
                // A faint tint rather than the surface colour: the rail is that
                // colour now, and the box would vanish into it.
                style: "margin-top:auto;padding:12px;display:flex;flex-direction:column;gap:5px;\
                        border-radius:8px;background-color:rgba(239,232,245,.03)",
                span {
                    style: "font-size:12px;letter-spacing:.1em;text-transform:uppercase;\
                            color:var(--color-neutral-500)",
                    "Движок"
                }
                span {
                    style: "font-size:14px",
                    if frame.engine.is_empty() { "не запущен" } else { "{frame.engine}" }
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
    let span_seconds = frame.history.len() / 2;
    let reason = frame.error.clone().unwrap_or_default();

    rsx! {
        div {
            style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:13px;\
                    padding:18px 20px;overflow:hidden",

            div { style: "display:flex;align-items:baseline;gap:10px",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Подключение" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;overflow:hidden;white-space:nowrap",
                    "{elide(&frame.server_tag, 40)}"
                }
            }

            // The reason a start failed. The engine has always carried it in the
            // frame and the window never read it, so a failure was a red ring
            // saying "Ошибка" and nothing else — the explanation existed only in
            // the log, which is one screen away and not where anyone looks first.
            if !reason.is_empty() {
                div {
                    style: "flex:none;display:flex;align-items:flex-start;gap:9px;\
                            padding:10px 13px;border-radius:8px;\
                            background-color:rgba(229,107,111,.10);\
                            box-shadow:inset 0 0 0 1px rgba(229,107,111,.26)",
                    span {
                        style: "font-size:13.5px;line-height:1.45;color:#f2a7ad;\
                                min-width:0;word-break:break-word",
                        "{reason}"
                    }
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
                        padding:15px;background-color:var(--color-surface);\
                        border-radius:8px;box-shadow:var(--shadow-sm)",

                TabBar {
                    tab,
                    meta: match tab() {
                        Tab::Traffic => rsx! {
                            div {
                                style: "display:flex;align-items:baseline;gap:16px;font-size:13.5px;\
                                        font-variant-numeric:tabular-nums",
                                span { style: "color:var(--color-neutral-600)",
                                    "последние {span_seconds} с"
                                }
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
                        padding:18px;background-color:var(--color-surface);\
                        border-radius:8px;box-shadow:var(--shadow-sm)",
                div {
                    class: "ring",
                    style: "width:78px;height:78px;flex:none;border-radius:50%;display:grid;\
                            place-items:center;border:1px solid {tone};\
                            box-shadow:0 0 34px {outer}, inset 0 0 20px {inner}",
                    title: if on { "Отключить" } else { "Подключить" },
                    onclick: move |_| controller.send(Command::Toggle { was_on: on }),
                    crate::icons::Power { size: 28, color: tone }
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
                        style: "font-size:14px;color:var(--color-neutral-500);\
                                font-variant-numeric:tabular-nums",
                        if on { "В сети {frame.uptime} · {frame.flow}" } else { "{frame.flow}" }
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
                        padding:14px;background-color:var(--color-surface);\
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

/// What the connection screen shows before there is a server: the one thing
/// bliss needs from anyone, asked for in the window rather than in a file.
#[component]
fn AddServerCard(frame: Frame) -> Element {
    rsx! {
        div {
            // The same night scene as the status card it stands in for.
            class: "status-card",
            style: "flex:none;display:flex;flex-direction:column;gap:14px;padding:20px 22px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            div { style: "display:flex;flex-direction:column;gap:6px;max-width:540px",
                span {
                    style: "font-weight:700;font-size:23px;line-height:1.15;\
                            font-family:var(--font-heading);color:var(--color-text)",
                    "Добавьте сервер"
                }
                span { style: "font-size:14px;line-height:1.5;color:var(--color-neutral-400)",
                    "Вставьте ссылку vless:// или адрес подписки — их даёт провайдер. \
                     bliss сохранит их в профиль и подключится. Можно просто нажать Ctrl+V."
                }
            }
            ImportForm { frame, placeholder: "vless://… или https://… подписки" }
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
fn ImportForm(frame: Frame, placeholder: &'static str) -> Element {
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

    rsx! {
        div { style: "display:flex;flex-direction:column;gap:7px;max-width:680px",
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
                            if let Some(text) = crate::clipboard::paste() {
                                let text = text.trim().to_owned();
                                link.set(text.clone());
                                submit(text);
                            }
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
        div { style: "display:flex;align-items:center;gap:6px",
            for one in [Tab::Traffic, Tab::Servers, Tab::Actions] {
                div {
                    key: "{one.key()}",
                    class: if tab() == one { "tab active" } else { "tab" },
                    style: "padding:5px 12px;border-radius:4px;\
                            font:700 14.5px/1.35 var(--font-heading)",
                    onclick: move |_| tab.set(one),
                    "{one.label()}"
                }
            }
            div { style: "margin-left:auto", {meta} }
        }
    }
}

#[component]
fn TrafficTab(frame: Frame) -> Element {
    let span_seconds = frame.history.len() / 2;
    rsx! {
        div { style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:9px",
            div { style: "flex:1;min-height:0;display:flex;align-items:flex-end;gap:3px",
                for value in frame.history.iter().copied() {
                    div {
                        class: "bar",
                        style: "flex:1;border-radius:1px 1px 0 0;\
                                height:{bar_height(value, frame.scale)}%;\
                                background-color:{bar_colour(value)}",
                    }
                }
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

    rsx! {
        div { style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:7px",
            div {
                style: "display:grid;{SERVER_COLUMNS};padding:0 10px 6px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);\
                        border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                span { "Имя" }
                span { "Адрес" }
                span { "Защита" }
                span { style: "text-align:right", "Отклик" }
                span {}
            }

            div { style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden",
                if frame.servers.is_empty() {
                    div { style: "padding:22px 10px;font-size:14.5px;color:var(--color-neutral-600)",
                        "Серверов пока нет — вставьте ссылку vless:// в поле ниже или нажмите Ctrl+V."
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

            ImportForm { frame: frame.clone(), placeholder: "vless://… или адрес подписки" }

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
                    padding:12px 13px;border-radius:8px;\
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
fn proxy_note(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::Unchanged => "bliss не трогает настройки Windows. Прокси прописываете сами.",
        ProxyMode::Set => {
            "Весь трафик Windows идёт через bliss, пока туннель поднят. \
                           При выходе настройки вернутся как были."
        }
        ProxyMode::Clear => {
            "Прокси в Windows выключен — на случай, если его оставила \
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
            style: "display:flex;align-items:center;gap:11px;padding:12px 13px;border-radius:8px;{tone}",
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
        Ping::Unknown | Ping::Probing => "var(--color-neutral-600)",
    }
}

#[component]
fn StatRow(label: String, value: String) -> Element {
    rsx! {
        div { style: "display:flex;justify-content:space-between;align-items:baseline;gap:10px",
            span { style: "font-size:13.5px;color:var(--color-neutral-500)", "{label}" }
            span {
                style: "font-size:15px;font-variant-numeric:tabular-nums;text-align:right",
                "{value}"
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

#[component]
fn Connections(frame: Frame) -> Element {
    const COLUMNS: &str = "grid-template-columns:1.6fr 1fr 78px 92px 92px;gap:10px";

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
    // The menus belong to this screen; leaving the screen closes them.
    use_drop(|| {
        *ROW_MENU.write() = None;
        *RULE_MENU.write() = false;
    });

    rsx! {
        div {
            // The same padding as the other screens, so the rail's card lines up
            // with this one's too.
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px",
            div { style: "display:flex;align-items:baseline;gap:10px",
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
                style: "display:grid;{COLUMNS};padding:0 8px 7px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                span { "Хост" }
                span { "Процесс" }
                span { "Маршрут" }
                SortHeader { label: "Приём", most: Sort::DownMost, least: Sort::DownLeast }
                SortHeader { label: "Отдача", most: Sort::UpMost, least: Sort::UpLeast }
            }

            div { style: "flex:1;min-height:90px;overflow-y:scroll;overflow-x:hidden",
                if frame.connections.is_empty() {
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
                        style: "display:grid;{COLUMNS};align-items:center;padding:7px 8px;\
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
                        span {
                            style: "font-size:14px;font-family:ui-monospace,Consolas,monospace",
                            "{elide(&row.target, 40)}"
                        }
                        span { style: "font-size:13.5px;color:var(--color-neutral-400)",
                            "{row.process.clone().unwrap_or_else(|| String::from(\"—\"))}"
                        }
                        span {
                            style: "font-size:12.5px;color:{route_color(row.closed_at.is_some())}",
                            if row.closed_at.is_some() { "закрыто" } else { "{route_short(row.route)}" }
                        }
                        span {
                            style: "text-align:right;font-size:13.5px;\
                                    font-variant-numeric:tabular-nums;color:var(--color-neutral-300)",
                            "{bytes(row.down)}"
                        }
                        span {
                            style: "text-align:right;font-size:13.5px;\
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
            RowMenuView { menu, copied }
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
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:13px;\
                    padding:18px 20px",

            div { style: "display:flex;align-items:baseline;gap:10px",
                h4 { style: "margin:0;font-size:21px;white-space:nowrap;flex:none", "Маршрутизация" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500);min-width:0;overflow:hidden;white-space:nowrap",
                    "{plural(view.rules.len() as u64, \"правило\", \"правила\", \"правил\")} · \
                     остальное {view.fallback.label()}"
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

            // No rules at all: offer the starting set rather than an empty list.
            if view.rules.is_empty() {
                div {
                    style: "flex:none;display:flex;align-items:center;gap:13px;padding:14px;\
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
                    style: "flex:none;display:flex;flex-direction:column;gap:5px;padding:11px 13px;\
                            border-radius:8px;\
                            background-color:rgba(203,186,244,.07);\
                            box-shadow:inset 0 0 0 1px rgba(203,186,244,.18)",
                    span {
                        style: "font-size:12px;letter-spacing:.09em;text-transform:uppercase;\
                                color:var(--color-neutral-500)",
                        "Правила, которые не сработают"
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
                RuleList { view: view.clone() }
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
            RuleEditor { draft }
        }
    }
}

#[component]
fn RuleList(view: crate::engine::RoutingView) -> Element {
    let controller = use_context::<Controller>();
    // The rule whose delete was clicked once; the second click deletes it.
    let mut armed = use_signal(|| None::<usize>);
    let count = view.rules.len();

    rsx! {
        div {
            style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:9px;padding:15px;\
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
                        class: "rule-row",
                        style: "display:flex;align-items:flex-start;gap:11px;padding:9px 10px;\
                                border-radius:4px",
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
                        div { style: "flex:none;display:flex;align-items:center;gap:8px",
                            ActionPill { action: rule.action }
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
                for action in [Action::Proxy, Action::Direct, Action::Block] {
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
                    padding:15px;background-color:var(--color-surface);\
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
                            padding:11px;border-radius:4px;background-color:rgba(238,130,196,.07);\
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
    };
    rsx! {
        span {
            style: "padding:3px 9px;border-radius:11px;font-size:12.5px;white-space:nowrap;\
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

/// Bars are a baseline when there is nothing to show and data when there is.
/// Height of one bar, as a percentage of the pane.
///
/// An idle slot gets a visible floor rather than a hairline. Forty hairlines
/// read as a chart that is broken; a row of low bars reads as a chart with no
/// traffic, which is what it actually is.
fn bar_height(value: u64, peak: u64) -> u64 {
    const FLOOR: u64 = 9;
    if value == 0 {
        return FLOOR;
    }
    (value * 100 / peak.max(1)).max(FLOOR + 7)
}

fn bar_colour(value: u64) -> &'static str {
    if value == 0 {
        "rgba(239,232,245,.07)"
    } else {
        "var(--color-accent-700)"
    }
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

    #[test]
    fn idle_chart_reads_as_a_baseline() {
        // Nothing happening: every bar sits on the same visible floor. The
        // floor was raised from a hairline because forty hairlines read as a
        // broken chart rather than as an idle one.
        assert_eq!(bar_height(0, 0), 9);
        assert_eq!(bar_height(0, 1_000), 9);
        assert_ne!(bar_colour(0), bar_colour(1));
        // A real sample always clears the floor, however small next to the peak.
        assert_eq!(bar_height(1, 1_000_000), 16);
        assert!(bar_height(1, 1_000_000) > bar_height(0, 1_000_000));
        assert_eq!(bar_height(500, 1_000), 50);
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

fn action_key(action: Action) -> &'static str {
    match action {
        Action::Proxy => "proxy",
        Action::Direct => "direct",
        Action::Block => "block",
    }
}

fn action_phrase(action: Action) -> &'static str {
    match action {
        Action::Proxy => "Через прокси",
        Action::Direct => "Напрямую",
        Action::Block => "Блокировать",
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
    }
}

fn route_word(route: bliss_core::Route) -> &'static str {
    match route {
        bliss_core::Route::Proxy => "через прокси",
        bliss_core::Route::Direct => "напрямую",
        bliss_core::Route::Blocked => "заблокировано",
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
fn RowMenuView(menu: RowMenu, mut copied: Signal<String>) -> Element {
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
    let tall = 12.0 + 27.0 + groups * (11.0 + 27.0 + 3.0 * 33.0) + 11.0 + 33.0;
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
                    for action in [Action::Proxy, Action::Direct, Action::Block] {
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
        Action::Direct | Action::Block => Action::Proxy,
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
    ("process:telegram.exe", "программа"),
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
fn RuleEditor(draft: Draft) -> Element {
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
                        div { style: "display:flex;gap:8px",
                            for action in [Action::Proxy, Action::Direct, Action::Block] {
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
const SERVER_COLUMNS: &str = "grid-template-columns:1.2fr 1.4fr 118px 78px 30px;gap:12px";

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
    let name = elide(&server.tag, 22);
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
            style: "display:grid;{SERVER_COLUMNS};align-items:center;padding:8px 10px 8px {left}px;\
                    border-radius:4px",
            onclick: move |_| select.send(Command::Select(index)),
            onmouseleave: move |_| {
                if armed() == Some(index) {
                    armed.set(None);
                }
            },
            span { style: "font-size:14.5px;min-width:0;overflow:hidden;white-space:nowrap", "{name}" }
            span {
                style: "font-size:13.5px;color:var(--color-neutral-400);\
                        font-family:ui-monospace,Consolas,monospace",
                "{elide(&server.addr, 26)}"
            }
            span { style: "font-size:13px;color:var(--color-neutral-500)", "{server.kind}" }
            span {
                style: "text-align:right;font-size:13.5px;\
                        font-variant-numeric:tabular-nums;color:{ping_tone(server.ping)}",
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

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:13px;\
                    padding:18px 20px",
            div { style: "display:flex;align-items:baseline;gap:10px",
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

            div {
                style: "flex:none;display:flex;flex-direction:column;gap:10px;padding:15px;\
                        background-color:var(--color-surface);border-radius:8px;\
                        box-shadow:var(--shadow-sm)",
                span { style: "font:700 16px/1 var(--font-heading)", "Добавить подписку" }
                span { style: "font-size:13px;line-height:1.5;color:var(--color-neutral-500)",
                    "Адрес списка серверов от провайдера. bliss загрузит список — через туннель, \
                     если он поднят, — и будет обновлять его раз в 12 часов. Ctrl+V в любом месте \
                     окна тоже добавляет."
                }
                ImportForm { frame: frame.clone(), placeholder: "https://…" }
            }

            div {
                style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;\
                        display:flex;flex-direction:column;gap:9px",
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
    let id = row.id.clone();

    rsx! {
        div {
            style: "flex:none;display:flex;align-items:flex-start;gap:13px;padding:14px;\
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
            div { style: "flex:none;display:flex;gap:6px",
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
