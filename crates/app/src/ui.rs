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

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;

use crate::engine::{Command, Controller, Frame, State, bytes, rate};

const NOCTURNE: &str = include_str!("../assets/nocturne.css");
const APP_CSS: &str = include_str!("../assets/app.css");

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
            Config::new().with_window(
                WindowBuilder::new()
                    .with_title("bliss")
                    // The design draws its own chrome, so Windows must not draw
                    // any. Everything the frame used to do — drag, minimise,
                    // maximise, close — is re-implemented in TitleBar below.
                    .with_decorations(false)
                    .with_inner_size(LogicalSize::new(1040.0, 660.0))
                    .with_min_inner_size(LogicalSize::new(880.0, 560.0)),
            ),
        )
        .with_context(controller)
        .launch(App);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Connection,
    Traffic,
}

impl Screen {
    fn key(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::Traffic => "traffic",
        }
    }
}

#[component]
fn App() -> Element {
    let controller = use_context::<Controller>();
    let mut frame = use_signal(|| controller.frame());
    let screen = use_signal(|| Screen::Connection);

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
        style { dangerous_inner_html: NOCTURNE }
        style { dangerous_inner_html: APP_CSS }
        // The title bar spans the whole window, as in the reference, with the
        // sidebar beginning underneath it.
        div {
            style: "width:100vw;height:100vh;display:flex;flex-direction:column;\
                    background:var(--color-bg);overflow:hidden;color:var(--color-text)",
            TitleBar { engine: frame.engine.clone() }
            div { style: "flex:1;min-height:0;display:flex",
                Sidebar { screen, frame: frame.clone() }
                // The key changes with the screen, which forces a fresh node
                // and lets the fade-in replay instead of being skipped.
                {
                    let key = screen().key();
                    match screen() {
                        Screen::Connection => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Connection { frame: frame.clone() }
                            }
                        },
                        Screen::Traffic => rsx! {
                            div { key: "{key}", class: "screen", style: SCREEN,
                                Connections { frame: frame.clone() }
                            }
                        },
                    }
                }
            }
        }
    }
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
fn Sidebar(screen: Signal<Screen>, frame: Frame) -> Element {
    let here = screen();
    rsx! {
        div {
            style: "width:210px;flex:none;display:flex;flex-direction:column;gap:2px;\
                    padding:12px 10px;border-right:1px solid var(--color-divider)",

            NavItem {
                label: "Подключение",
                active: here == Screen::Connection,
                enabled: true,
                onclick: move |_| screen.set(Screen::Connection),
                icon: rsx! {
                    crate::icons::Power { size: 18, color: nav_tint(here == Screen::Connection, true) }
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

            // The reference's other destinations exist in the design but not yet
            // in the program. Shown so the shape is right, dimmed and inert so
            // nothing claims to work that does not.
            NavItem {
                label: "Серверы",
                count: frame.servers,
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Globe { size: 18, color: nav_tint(false, false) } },
            }
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
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Split { size: 18, color: nav_tint(false, false) } },
            }
            NavItem {
                label: "Настройки",
                active: false,
                enabled: false,
                onclick: move |_| {},
                icon: rsx! { crate::icons::Gear { size: 18, color: nav_tint(false, false) } },
            }

            div {
                style: "margin-top:auto;padding:12px;display:flex;flex-direction:column;gap:5px;\
                        border-radius:var(--radius-md);background:var(--color-surface)",
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
    let class = match (active, enabled) {
        (true, _) => "nav-item active",
        (false, true) => "nav-item",
        (false, false) => "nav-item disabled",
    };
    let decoration = if active {
        "color:var(--color-accent);box-shadow:inset 2px 0 0 var(--color-accent);\
         background-color:rgba(145,132,217,.10);"
    } else if enabled {
        "color:var(--color-neutral-300);"
    } else {
        "color:var(--color-neutral-700);"
    };
    rsx! {
        div {
            class: "{class}",
            style: "display:flex;align-items:center;gap:10px;padding:9px 10px;\
                    border-radius:var(--radius-md);font-size:15px;cursor:default;{decoration}",
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
fn Connection(frame: Frame) -> Element {
    let controller = use_context::<Controller>();
    let (tone, outer, inner) = tones(frame.state);
    let on = frame.state.is_on();
    let span_seconds = frame.history.len() / 2;

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

            div { style: "display:flex;gap:13px;align-items:stretch;flex:none",
                div {
                    style: "flex:1;min-width:0;display:flex;align-items:center;gap:18px;\
                            padding:18px;background:var(--color-surface);\
                            border-radius:var(--radius-md);box-shadow:var(--shadow-sm)",
                    div {
                        class: "ring",
                        style: "width:78px;height:78px;flex:none;border-radius:50%;display:grid;\
                                place-items:center;border:1px solid {tone};cursor:default;\
                                box-shadow:0 0 34px {outer}, inset 0 0 20px {inner}",
                        onclick: move |_| controller.send(Command::Toggle),
                        crate::icons::Power { size: 28, color: tone }
                    }
                    div { style: "display:flex;flex-direction:column;gap:4px;min-width:0",
                        span {
                            style: "font:500 25px/1.1 var(--font-heading);color:{tone}",
                            "{frame.state.label()}"
                        }
                        span { style: "font-size:14px;color:var(--color-neutral-400)",
                            "{elide(&frame.sni, 26)} · {frame.server_addr}"
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
                                move |_| controller.send(Command::Toggle)
                            },
                        }
                        span { style: "font-size:13px;color:var(--color-neutral-600)",
                            "{frame.listen}"
                        }
                    }
                }

                div {
                    style: "width:232px;flex:none;display:flex;flex-direction:column;gap:10px;\
                            padding:14px;background:var(--color-surface);\
                            border-radius:var(--radius-md);box-shadow:var(--shadow-sm)",
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

            div {
                style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:11px;\
                        padding:15px;background:var(--color-surface);\
                        border-radius:var(--radius-md);box-shadow:var(--shadow-sm)",
                div { style: "display:flex;align-items:baseline;gap:12px",
                    span { style: "font:500 16px/1 var(--font-heading)", "Трафик" }
                    span { style: "font-size:13.5px;color:var(--color-neutral-500)",
                        "последние {span_seconds} с"
                    }
                    div {
                        style: "margin-left:auto;display:flex;gap:16px;font-size:13.5px;\
                                font-variant-numeric:tabular-nums",
                        span { style: "color:var(--color-accent-300)", "↓ {rate(frame.down_rate)}" }
                        span { style: "color:var(--color-neutral-400)", "↑ {rate(frame.up_rate)}" }
                    }
                }
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

    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;gap:12px;\
                    padding:18px 24px 20px",
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
            }

            div {
                style: "display:grid;{COLUMNS};padding:0 8px 7px;font-size:12px;\
                        letter-spacing:.09em;text-transform:uppercase;\
                        color:var(--color-neutral-500);border-bottom:1px solid var(--color-divider)",
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
                                border-bottom:1px solid rgba(233,233,237,.05)",
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
        }

            LogStrip {}
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
    rsx! {
        div {
            style: "flex:1;min-height:150px;display:flex;flex-direction:column;gap:9px",
            div { style: "display:flex;align-items:center;gap:10px",
                span { style: "font:500 14px/1 var(--font-heading)", "Журнал" }
                span { class: "tag tag-accent", style: "font-size:11px;padding:2px 8px", "live" }
                span {
                    style: "margin-left:auto;font-size:12.5px;color:var(--color-neutral-600)",
                    "новые сверху"
                }
            }
            div {
                style: "flex:1;min-height:0;overflow-y:scroll;overflow-x:hidden;\
                        background:#12131d;border-radius:var(--radius-sm);padding:9px 12px;\
                        display:flex;flex-direction:column;gap:4px;\
                        font-family:ui-monospace,Consolas,monospace;font-size:12.5px;line-height:1.5",
                if lines.is_empty() {
                    span { style: "color:var(--color-neutral-700)", "пока тихо" }
                }
                for line in lines.iter() {
                    div { style: "display:flex;gap:10px;align-items:baseline",
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
