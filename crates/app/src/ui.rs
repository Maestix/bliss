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
use bliss_proto::ProxyMode;

const NOCTURNE: &str = include_str!("../assets/nocturne.css");
const APP_CSS: &str = include_str!("../assets/app.css");
/// The status card's night scene; see `.status-card` in app.css.
const SCENE: &[u8] = include_bytes!("../assets/status-card.jpg");
/// How app.css names the scene, for `STYLES` to replace.
const SCENE_URL: &str = "url(status-card.jpg)";

/// app.css with the scene inlined as a data URI, so the picture ships inside
/// bliss.exe the way the stylesheets do. Built once: `App` renders twice a
/// second and should not encode a picture every time.
static STYLES: LazyLock<String> = LazyLock::new(|| {
    let data = base64::engine::general_purpose::STANDARD.encode(SCENE);
    APP_CSS.replace(SCENE_URL, &format!("url(data:image/jpeg;base64,{data})"))
});

/// This file, so a test can check the styles in it rather than trust a rule to
/// be remembered. See `radius_literals_match_the_design_tokens`.
#[cfg(test)]
const SOURCE: &str = include_str!("ui.rs");

/// The wrapper every screen sits in, so the fade-in has something to animate.
const SCREEN: &str = "flex:1;min-height:0;display:flex;flex-direction:column";
const ACCENT: &str = "#9184d9";
/// Neutral-400: window controls and anything else that should recede.
const MUTED: &str = "#b2b6ca";

// Status is carried by colour rather than by a word, so these three are the
// only place the state is spelled out. Muted on purpose: a saturated green
// against this ground would read as a different product.
const ON: (&str, &str, &str) = ("#5aa87f", "rgba(90,168,127,.24)", "rgba(90,168,127,.12)");
const OFF: (&str, &str, &str) = ("#a8505c", "rgba(168,80,92,.20)", "rgba(168,80,92,.10)");
// Starting gets the accent rather than a premature green: it is not connected
// yet, and saying so for the second it lasts costs nothing.
const BUSY: (&str, &str, &str) = (ACCENT, "rgba(145,132,217,.20)", "rgba(145,132,217,.10)");

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
    Routing,
}

impl Screen {
    fn key(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Traffic => "traffic",
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

    let frame = frame();

    rsx! {
        Styles {}
        WindowIcons {}
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
/// which renders twice a second, every render copied about 44 KB of CSS into
/// fresh strings only to find them unchanged.
#[component]
fn Styles() -> Element {
    rsx! {
        style { dangerous_inner_html: NOCTURNE }
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
            // so its edges line up with theirs.
            style: "width:200px;flex:none;display:flex;flex-direction:column;gap:2px;\
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

            // The reference's other destinations exist in the design but not yet
            // in the program. Shown so the shape is right, dimmed and inert so
            // nothing claims to work that does not.
            NavItem {
                label: "Подписки",
                count: 0,
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Rss { size: 18, color: nav_tint(false, false) } },
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
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Gear { size: 18, color: nav_tint(false, false) } },
            }

            div {
                // A faint tint rather than the surface colour: the rail is that
                // colour now, and the box would vanish into it.
                style: "margin-top:auto;padding:12px;display:flex;flex-direction:column;gap:5px;\
                        border-radius:8px;background-color:rgba(233,233,237,.03)",
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
            style: "display:flex;align-items:center;gap:10px;padding:9px 10px;\
                    border-radius:8px;font-size:15px;cursor:default",
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
        (false, true) => "#b2b6ca",
        (false, false) => "#595d6c",
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
                h4 { style: "margin:0;font-size:21px", "Подключение" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500)",
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
                            background-color:rgba(168,80,92,.10);\
                            box-shadow:inset 0 0 0 1px rgba(168,80,92,.26)",
                    span {
                        style: "font-size:13.5px;line-height:1.45;color:#d9a2a9;\
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
                            place-items:center;border:1px solid {tone};cursor:default;\
                            box-shadow:0 0 34px {outer}, inset 0 0 20px {inner}",
                    onclick: move |_| controller.send(Command::Toggle { was_on: on }),
                    crate::icons::Power { size: 28, color: tone }
                }
                div { style: "display:flex;flex-direction:column;gap:4px;min-width:0",
                    span {
                        // Longhands, not the `font:` shorthand: see the
                        // note at the top of this file.
                        style: "font-weight:500;font-size:25px;line-height:1.1;\
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
                div {
                    style: "margin-left:auto;display:flex;flex-direction:column;\
                            align-items:flex-end;gap:10px",
                    Toggle {
                        on,
                        tone: tone.to_owned(),
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Toggle { was_on: on })
                        },
                    }
                    span { style: "font-size:13px;color:var(--color-neutral-600)",
                        "{frame.listen}"
                    }
                }
            }

            div {
                style: "width:232px;flex:none;display:flex;flex-direction:column;gap:10px;\
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
                    style: "font-weight:500;font-size:23px;line-height:1.15;\
                            font-family:var(--font-heading);color:var(--color-text)",
                    "Добавьте сервер"
                }
                span { style: "font-size:14px;line-height:1.5;color:var(--color-neutral-400)",
                    "Вставьте ссылку vless:// — её даёт провайдер или страница подписки. \
                     bliss сохранит её в профиль и подключится."
                }
            }
            AddServerForm { frame }
        }
    }
}

/// Paste a link, get a server.
///
/// The typed text lives in a local signal rather than in the frame, which is
/// republished twice a second and would fight whoever is typing. The engine's
/// answer comes back in `frame.added`; its `attempt` tells a fresh answer from
/// the one already on screen when this form appeared, so switching tabs does
/// not replay an old "added".
#[component]
fn AddServerForm(frame: Frame) -> Element {
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
            controller.send(Command::AddServer(Link(text)));
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
                    placeholder: "vless://…",
                    autocomplete: "off",
                    spellcheck: "false",
                    style: "flex:1;min-width:0;padding:8px 10px;border-radius:4px;border:none;\
                            background-color:#12131d;color:var(--color-text);font-size:14px;\
                            font-family:ui-monospace,Consolas,monospace;\
                            box-shadow:inset 0 0 0 1px rgba(233,233,237,.10)",
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
                Some(Ok(tag)) => rsx! {
                    span { style: "font-size:13px;color:#5aa87f", "Добавлен «{elide(&tag, 40)}»" }
                },
                Some(Err(why)) => rsx! {
                    span {
                        style: "font-size:13px;line-height:1.45;color:#d9a2a9;word-break:break-word",
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
                    style: "padding:5px 12px;border-radius:4px;cursor:default;\
                            font:500 14.5px/1.35 var(--font-heading)",
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
    const COLUMNS: &str = "grid-template-columns:1.2fr 1.4fr 118px 78px;gap:12px";
    let controller = use_context::<Controller>();
    let probing = sweeping(&frame);

    rsx! {
        div { style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:7px",
            div {
                style: "display:grid;{COLUMNS};padding:0 10px 6px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);\
                        border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                span { "Имя" }
                span { "Адрес" }
                span { "Защита" }
                span { style: "text-align:right", "Отклик" }
            }

            div { style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden",
                if frame.servers.is_empty() {
                    div { style: "padding:22px 10px;font-size:14.5px;color:var(--color-neutral-600)",
                        "Серверов пока нет — вставьте ссылку vless:// в поле ниже."
                    }
                }
                for (index, server) in frame.servers.iter().enumerate() {
                    div {
                        key: "{index}",
                        class: if server.current { "srv current" } else { "srv" },
                        // Marked by class, not by inline style: see the note in
                        // NavItem. A property that vanishes from a style string
                        // is restored by the renderer, so the previous server
                        // would keep its marker after a switch.
                        style: "display:grid;{COLUMNS};align-items:center;padding:8px 10px;\
                                border-radius:4px;cursor:default",
                        onclick: {
                            let controller = controller.clone();
                            move |_| controller.send(Command::Select(index))
                        },
                        span { style: "font-size:14.5px", "{elide(&server.tag, 22)}" }
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
                    }
                }
            }

            AddServerForm { frame: frame.clone() }

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

    rsx! {
        div {
            style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;display:grid;\
                    grid-template-columns:1fr 1fr;gap:9px;align-content:start;padding-top:2px",

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
                    background-color:rgba(233,233,237,.03)",
            div { style: "display:flex;align-items:center;gap:11px",
                crate::icons::Shield { size: 17, color: ACCENT }
                span { style: "font-size:14.5px", "Системный прокси" }
                span {
                    style: "margin-left:auto;font-size:12.5px;color:{state.1}",
                    "{state.0}"
                }
            }
            div { style: "display:flex;flex-direction:column;gap:3px",
                for mode in [ProxyMode::Unchanged, ProxyMode::Set, ProxyMode::Clear] {
                    div {
                        key: "{mode.key()}",
                        class: if mode == chosen { "act choice on" } else { "act choice" },
                        style: "display:flex;align-items:center;gap:9px;padding:6px 9px;border-radius:4px;\
                                cursor:default;font-size:13.5px;\
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
        "rgba(233,233,237,.22)"
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
            class: if enabled { "act" } else { "act disabled" },
            style: "display:flex;align-items:center;gap:11px;padding:12px 13px;cursor:default;border-radius:8px;\
                    background-color:rgba(233,233,237,.03);{tone}",
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
        "color:var(--color-neutral-200);box-shadow:inset 0 0 0 1px rgba(145,132,217,.35)"
    } else {
        "color:var(--color-neutral-700);box-shadow:inset 0 0 0 1px rgba(233,233,237,.07)"
    };
    rsx! {
        div {
            class: if enabled { "act" } else { "act disabled" },
            style: "display:inline-flex;align-items:center;gap:7px;padding:6px 13px;border-radius:4px;\
                    cursor:default;font-size:14px;{tone}",
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
        Ping::Ok(ms) if ms < 120 => ON.0,
        Ping::Ok(ms) if ms < 300 => "var(--color-accent-300)",
        Ping::Ok(_) => "var(--color-neutral-400)",
        Ping::Down => OFF.0,
        Ping::Unknown | Ping::Probing => "var(--color-neutral-600)",
    }
}

/// The pill switch from the reference. It does the same thing as the ring:
/// the design offers both, and the switch is the one people reach for.
#[component]
fn Toggle(on: bool, tone: String, onclick: EventHandler<MouseEvent>) -> Element {
    let shift = if on { "23px" } else { "0px" };
    rsx! {
        div {
            style: "width:52px;height:28px;border-radius:14px;padding:2px;display:flex;\
                    cursor:default;box-shadow:inset 0 0 0 1px {tone};\
                    background-color:rgba(145,132,217,.08)",
            onclick: move |e| onclick.call(e),
            div {
                style: "width:24px;height:24px;border-radius:50%;background-color:{tone};\
                        transform:translateX({shift});transition:transform .18s ease",
            }
        }
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
        State::Stopped | State::Failed => OFF,
    }
}

#[component]
fn Connections(frame: Frame) -> Element {
    const COLUMNS: &str = "grid-template-columns:1.6fr 1fr 78px 92px 92px;gap:10px";

    // Same click-to-copy as the log below it, and the same note.
    let mut copied = use_signal(String::new);
    let note = copied();

    rsx! {
        div {
            // The same padding as the other screens, so the rail's card lines up
            // with this one's too.
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 20px",
            div { style: "display:flex;align-items:baseline;gap:10px",
                h4 { style: "margin:0;font-size:21px", "Логи и трафик" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500)",
                    "{plural(frame.active, \"соединение\", \"соединения\", \"соединений\")} сейчас · {frame.opened} за сессию"
                }
                span {
                    style: "margin-left:auto;font-size:13.5px;color:var(--color-neutral-500);\
                            font-variant-numeric:tabular-nums",
                    "↓ {rate(frame.down_rate)}   ↑ {rate(frame.up_rate)}"
                }
                Button {
                    label: "Скопировать таблицу",
                    enabled: !frame.connections.is_empty(),
                    onclick: {
                        let rows = frame.connections.clone();
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

            if !note.is_empty() {
                span {
                    style: "flex:none;font-size:12.5px;color:var(--color-neutral-600)",
                    "{note}"
                }
            }

            div {
                style: "display:grid;{COLUMNS};padding:0 8px 7px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);border-bottom-width:1px;border-bottom-style:solid;border-bottom-color:var(--color-divider)",
                span { "Хост" }
                span { "Процесс" }
                span { "Маршрут" }
                span { style: "text-align:right", "Приём" }
                span { style: "text-align:right", "Отдача" }
            }

            div { style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden",
                if frame.connections.is_empty() {
                    div {
                        style: "padding:28px 8px;font-size:14.5px;color:var(--color-neutral-600)",
                        "Пока пусто. Направьте что-нибудь в socks5://127.0.0.1:{frame.listen.split(' ').nth(1).unwrap_or_default()}"
                    }
                }
                for row in frame.connections.iter() {
                    div {
                        key: "{row.id}",
                        class: "row",
                        style: "display:grid;{COLUMNS};align-items:center;padding:7px 8px;\
                                cursor:default;border-bottom:1px solid rgba(233,233,237,.05)",
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
                        span {
                            style: "font-size:14px;font-family:ui-monospace,Consolas,monospace",
                            "{elide(&row.target, 40)}"
                        }
                        span { style: "font-size:13.5px;color:var(--color-neutral-400)",
                            "{row.process.clone().unwrap_or_else(|| String::from(\"—\"))}"
                        }
                        span {
                            style: "font-size:12.5px;color:{route_color(row.closed_at.is_some())}",
                            if row.closed_at.is_some() { "закрыто" } else { "{row.route}" }
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
            LogStrip {}
        }
    }
}

// --- screen: routing ------------------------------------------------------

/// The rule table, and a box for asking it questions.
///
/// Shows, explains and tests the rules. Changing them in the window comes with
/// the rule editor; until then the one change offered is the starting set.
#[component]
fn Routing(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let view = &frame.routing;
    let query = use_signal(String::new);

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:13px;\
                    padding:18px 20px",

            div { style: "display:flex;align-items:baseline;gap:10px",
                h4 { style: "margin:0;font-size:21px", "Маршрутизация" }
                span { style: "font-size:13.5px;color:var(--color-neutral-500)",
                    "{plural(view.rules.len() as u64, \"правило\", \"правила\", \"правил\")} · \
                     остальное {view.fallback.label()}"
                }
            }

            // No rules at all: offer the starting set rather than an empty list.
            if view.rules.is_empty() {
                div {
                    style: "flex:none;display:flex;align-items:center;gap:13px;padding:14px;\
                            border-radius:8px;background-color:var(--color-surface);\
                            box-shadow:inset 0 0 0 1px rgba(145,132,217,.24)",
                    div { style: "display:flex;flex-direction:column;gap:3px;min-width:0",
                        span { style: "font-size:14.5px", "Правил пока нет" }
                        span {
                            style: "font-size:13px;line-height:1.45;color:var(--color-neutral-500)",
                            "Сейчас всё идёт через прокси. Можно добавить стартовый набор: \
                             реклама блокируется, локальная сеть идёт напрямую."
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
                            background-color:rgba(210,206,253,.07);\
                            box-shadow:inset 0 0 0 1px rgba(210,206,253,.18)",
                    span {
                        style: "font-size:12px;letter-spacing:.09em;text-transform:uppercase;\
                                color:var(--color-neutral-500)",
                        "Правила, которые не сработают"
                    }
                    for (n, warning) in view.warnings.iter().enumerate() {
                        span {
                            key: "{n}",
                            style: "font-size:13px;line-height:1.45;color:#d2cefd;\
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
                    onask: {
                        let controller = controller.clone();
                        move |text: String| controller.send(Command::Check(text))
                    },
                }
            }
        }
    }
}

#[component]
fn RuleList(view: crate::engine::RoutingView) -> Element {
    rsx! {
        div {
            style: "flex:1;min-width:0;display:flex;flex-direction:column;gap:9px;padding:15px;\
                    background-color:var(--color-surface);border-radius:8px;\
                    box-shadow:var(--shadow-sm)",
            div { style: "display:flex;align-items:baseline;gap:10px",
                span { style: "font:500 16px/1 var(--font-heading)", "Правила" }
                span { style: "font-size:13px;color:var(--color-neutral-600)",
                    "сверху вниз, срабатывает первое"
                }
            }

            div {
                style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;\
                        display:flex;flex-direction:column;gap:5px",
                if view.rules.is_empty() {
                    span {
                        style: "padding:14px 2px;font-size:14px;color:var(--color-neutral-600)",
                        "Ни одного правила — значит решает только строка «остальное»."
                    }
                }
                for rule in view.rules.iter() {
                    div {
                        key: "{rule.position}",
                        style: "display:flex;align-items:flex-start;gap:11px;padding:9px 10px;\
                                border-radius:4px;\
                                background-color:rgba(233,233,237,.03)",
                        // The number is the rule: first match wins, so the
                        // order is not decoration.
                        span {
                            style: "flex:none;width:18px;font-size:13px;text-align:right;\
                                    color:var(--color-neutral-700);\
                                    font-variant-numeric:tabular-nums",
                            "{rule.position}"
                        }
                        div { style: "display:flex;flex-direction:column;gap:3px;min-width:0",
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
                        div { style: "margin-left:auto;flex:none", ActionPill { action: rule.action } }
                    }
                }
            }

            div {
                style: "display:flex;align-items:center;gap:9px;padding-top:2px;\
                        border-top-width:1px;border-top-style:solid;border-top-color:var(--color-divider)",
                span {
                    style: "padding-top:8px;font-size:13.5px;color:var(--color-neutral-500)",
                    "Всё остальное"
                }
                div { style: "margin-left:auto;padding-top:8px",
                    ActionPill { action: view.fallback }
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
    onask: EventHandler<String>,
) -> Element {
    rsx! {
        div {
            style: "width:288px;flex:none;display:flex;flex-direction:column;gap:11px;\
                    padding:15px;background-color:var(--color-surface);\
                    border-radius:8px;box-shadow:var(--shadow-sm)",
            span { style: "font:500 16px/1 var(--font-heading)", "Проверить адрес" }
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
                        background-color:#12131d;color:var(--color-text);font-size:14px;\
                        font-family:ui-monospace,Consolas,monospace;\
                        box-shadow:inset 0 0 0 1px rgba(233,233,237,.10)",
                // The text lives here rather than in the frame: a controlled
                // input fed from a value that is republished twice a second
                // fights whoever is typing into it.
                oninput: move |e| {
                    let text = e.value();
                    query.set(text.clone());
                    onask.call(text);
                },
            }

            if let Some(answer) = result {
                div {
                    style: "display:flex;flex-direction:column;gap:7px;padding:11px;\
                            border-radius:4px;background-color:rgba(233,233,237,.03)",
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
        bliss_proto::Action::Proxy => ("rgba(145,132,217,.14)", ACCENT),
        bliss_proto::Action::Direct => ("rgba(90,168,127,.14)", ON.0),
        bliss_proto::Action::Block => ("rgba(168,80,92,.14)", OFF.0),
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
fn plural(n: u64, one: &str, few: &str, many: &str) -> String {
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
        "rgba(233,233,237,.07)"
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
        const SURFACE: [u8; 3] = [0x23, 0x25, 0x32];
        assert!(
            NOCTURNE.contains("--color-surface: #232532;"),
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
            // The toggle's pill.
            ("--radius-lg", "14px"),
        ] {
            let declaration = format!("{token}: {literal};");
            assert!(
                NOCTURNE.contains(&declaration),
                "nocturne no longer says {declaration:?} — the inline radii in this file \
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
/// pane is worth.
#[component]
fn LogStrip() -> Element {
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
            style: "flex:1;min-height:150px;display:flex;flex-direction:column;gap:9px",
            div { style: "display:flex;align-items:center;gap:10px",
                span { style: "font:500 14px/1 var(--font-heading)", "Журнал" }
                span { class: "tag tag-accent", style: "font-size:11px;padding:2px 8px", "live" }
                span {
                    style: "margin-left:auto;font-size:12.5px;color:var(--color-neutral-600)",
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
                        background-color:#12131d;border-radius:4px;padding:9px 12px;\
                        display:flex;flex-direction:column;gap:4px;\
                        font-family:ui-monospace,Consolas,monospace;font-size:12.5px;line-height:1.5",
                if lines.is_empty() {
                    span { style: "color:var(--color-neutral-700)", "пока тихо" }
                }
                for (n, line) in lines.iter().enumerate() {
                    div {
                        key: "{n}",
                        class: "logline",
                        style: "display:flex;gap:10px;align-items:baseline;cursor:default;\
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
