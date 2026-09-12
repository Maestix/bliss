//! Stage 1.0 — the renderer measurement.
//!
//! Screen 1c from the mockups, built once and launched under both targets:
//!
//!     cargo run -p bliss-spike --release                                  # WebView2
//!     cargo run -p bliss-spike --release --no-default-features -F native  # Blitz
//!
//! It is the most effect-heavy screen in the set on purpose — a glowing ring,
//! a fading rule, tabular figures, thirty gradient-filled bars, inset shadows.
//! What we want out of it is resident memory, process count, and a list of what
//! Blitz does not draw. The design system underneath is the real `nocturne.css`,
//! copied rather than approximated, because approximating it would measure a
//! different page than the one we intend to ship.

use dioxus::prelude::*;

const NOCTURNE: &str = include_str!("../assets/nocturne.css");

/// Stamped into the title bar so a screenshot says which build it came from.
#[cfg(feature = "desktop")]
const RENDERER: &str = "webview2";
#[cfg(all(feature = "native", not(feature = "desktop")))]
const RENDERER: &str = "blitz";

const ACCENT: &str = "#9184d9";

/// The mockup's sample traffic, kept verbatim so the two builds and the
/// original artboard can be compared bar for bar.
const BARS: [f32; 30] = [
    0.34, 0.52, 0.41, 0.66, 0.58, 0.72, 0.49, 0.38, 0.61, 0.83, 0.70, 0.55, 0.44, 0.62, 0.79, 0.91,
    0.68, 0.50, 0.39, 0.47, 0.64, 0.76, 0.58, 0.42, 0.53, 0.69, 0.87, 0.60, 0.45, 0.36,
];

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        style { dangerous_inner_html: NOCTURNE }
        div {
            style: "width:100vw;height:100vh;display:flex;background:var(--color-bg);\
                    overflow:hidden;color:var(--color-text)",
            Rail {}
            div { style: "flex:1;min-width:0;display:flex;flex-direction:column",
                TitleBar {}
                Hero {}
            }
        }
    }
}

/// The icon rail. Six destinations, so in the real shell this grows labels
/// (direction 1a); here it is the strip of inset rings and tinted fills.
#[component]
fn Rail() -> Element {
    rsx! {
        div {
            style: "width:66px;flex:none;display:flex;flex-direction:column;align-items:center;\
                    gap:6px;padding:14px 0;background:#12131d",
            div {
                style: "width:34px;height:34px;border-radius:var(--radius-md);display:grid;\
                        place-items:center;box-shadow:inset 0 0 0 1px var(--color-accent-700);\
                        margin-bottom:10px",
                Shield {}
            }
            RailSlot { active: true, Power { size: 18, color: ACCENT } }
            RailSlot { active: false, Globe {} }
            RailSlot { active: false, Split {} }
            div { style: "margin-top:auto", RailSlot { active: false, Gear {} } }
        }
    }
}

#[component]
fn RailSlot(active: bool, children: Element) -> Element {
    let fill = if active {
        "background:rgba(145,132,217,.12);"
    } else {
        ""
    };
    rsx! {
        div {
            style: "width:40px;height:36px;display:grid;place-items:center;\
                    border-radius:var(--radius-md);{fill}",
            {children}
        }
    }
}

#[component]
fn TitleBar() -> Element {
    rsx! {
        div {
            style: "display:flex;align-items:center;height:38px;flex:none;padding-left:20px",
            span {
                style: "font-size:11px;letter-spacing:.14em;text-transform:uppercase;\
                        color:var(--color-neutral-600)",
                "bliss · {RENDERER}"
            }
            div { style: "margin-left:auto;display:flex",
                WindowButton { glyph: "\u{2013}" }
                WindowButton { glyph: "\u{25a1}" }
                WindowButton { glyph: "\u{00d7}" }
            }
        }
    }
}

#[component]
fn WindowButton(glyph: String) -> Element {
    rsx! {
        div {
            style: "width:46px;height:38px;display:grid;place-items:center;\
                    color:var(--color-neutral-500);font-size:13px",
            "{glyph}"
        }
    }
}

#[component]
fn Hero() -> Element {
    rsx! {
        div {
            style: "flex:1;min-height:0;display:flex;flex-direction:column;padding:22px 44px 26px",

            span {
                style: "font-size:11px;letter-spacing:.16em;text-transform:uppercase;\
                        color:var(--color-accent)",
                "Reality · Vision · TCP 443"
            }
            div { style: "display:flex;align-items:flex-end;gap:26px;margin-top:10px",
                h1 { style: "margin:0;font-size:60px;letter-spacing:-.03em;line-height:1",
                    "Подключено"
                }
                span {
                    style: "font-size:13px;color:var(--color-neutral-500);padding-bottom:10px;\
                            font-variant-numeric:tabular-nums",
                    "02:14:36"
                }
            }

            // The Nocturne signature rule: fades to transparent at both ends.
            // One of the things Blitz may not paint — its gradient support is
            // the open question this spike answers.
            div {
                style: "height:1px;margin:22px 0 24px;background:linear-gradient(to right,\
                        transparent,var(--color-divider) 48px,\
                        var(--color-divider) calc(100% - 48px),transparent)",
            }

            div { style: "display:flex;gap:44px;align-items:flex-start",
                Stat { label: "Сервер", value: "ams-01", note: "Нидерланды · 34 мс" }
                Stat { label: "Приём", value: "12,4", unit: " МБ/с", note: "за сессию 4,82 ГБ" }
                Stat { label: "Отдача", value: "1,2", unit: " МБ/с", note: "за сессию 612 МБ" }
                PowerRing {}
            }

            div {
                style: "flex:1;min-height:0;display:flex;align-items:flex-end;gap:3px;margin:26px 0 0",
                for v in BARS {
                    div {
                        style: "flex:1;border-radius:1px;background:var(--color-accent-700);\
                                height:{(v * 100.0).round()}%",
                    }
                }
            }

            div { style: "display:flex;align-items:center;gap:10px;margin-top:20px",
                span { class: "tag tag-outline", style: "font-size:10.5px", "TUN" }
                span { class: "tag tag-neutral", style: "font-size:10.5px",
                    "обход RU · 1284 правила"
                }
                span { class: "tag tag-neutral", style: "font-size:10.5px", "kill-switch" }
                span {
                    style: "margin-left:auto;font-size:11.5px;color:var(--color-neutral-600)",
                    "socks 10808 · http 10809"
                }
            }
        }
    }
}

#[component]
fn Stat(label: String, value: String, unit: Option<String>, note: String) -> Element {
    rsx! {
        div { style: "display:flex;flex-direction:column;gap:6px",
            span {
                style: "font-size:11px;letter-spacing:.1em;text-transform:uppercase;\
                        color:var(--color-neutral-600)",
                "{label}"
            }
            span {
                style: "font:500 22px/1.1 var(--font-heading);font-variant-numeric:tabular-nums",
                "{value}"
                if let Some(unit) = unit {
                    span { style: "font-size:12px;color:var(--color-neutral-500)", "{unit}" }
                }
            }
            span { style: "font-size:12px;color:var(--color-neutral-500)", "{note}" }
        }
    }
}

/// The centrepiece: a ring lit from inside and out. Two box-shadows, one of
/// them inset — if either is missing the screen loses its focal point.
#[component]
fn PowerRing() -> Element {
    rsx! {
        div {
            style: "margin-left:auto;display:flex;flex-direction:column;align-items:flex-end;gap:12px",
            div {
                style: "width:132px;height:132px;border-radius:50%;display:grid;place-items:center;\
                        border:1px solid var(--color-accent);\
                        box-shadow:0 0 44px rgba(145,132,217,.20), inset 0 0 26px rgba(145,132,217,.10)",
                Power { size: 38, color: ACCENT }
            }
            span { style: "font-size:11.5px;color:var(--color-neutral-500)", "Отключить" }
        }
    }
}

// --- icons ----------------------------------------------------------------
//
// Inline SVG with colours as attributes, not CSS. Blitz renders SVG but does
// not style it, so `currentColor` and class-driven fills would work under
// WebView2 and quietly vanish under Blitz — exactly the kind of difference a
// spike is for. Attributes keep both honest.

#[component]
fn Power(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: "1.7", stroke_linecap: "round",
            path { d: "M12 3.2 V12.4" }
            path { d: "M7.4 6.6 A7.6 7.6 0 1 0 16.6 6.6" }
        }
    }
}

#[component]
fn Shield() -> Element {
    rsx! {
        svg {
            width: "17", height: "17", view_box: "0 0 24 24", fill: "none", stroke: ACCENT,
            stroke_width: "1.7", stroke_linejoin: "round",
            path { d: "M12 2.6 20 5.6 V12 c0 5-3.6 8-8 9.4 C8 20 4.4 17 4.4 12 V5.6 Z" }
            path { d: "M8.6 11.8 11.2 14.4 15.6 9.6", stroke_linecap: "round" }
        }
    }
}

#[component]
fn Globe() -> Element {
    rsx! {
        svg {
            width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "#75798c",
            stroke_width: "1.6",
            circle { cx: "12", cy: "12", r: "8.6" }
            path { d: "M3.4 12 H20.6" }
            path { d: "M12 3.4 a13 13 0 0 1 0 17.2 a13 13 0 0 1 0-17.2" }
        }
    }
}

#[component]
fn Split() -> Element {
    rsx! {
        svg {
            width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "#75798c",
            stroke_width: "1.6", stroke_linecap: "round",
            path { d: "M4 19 C10 19 8 5 20 5" }
            path { d: "M4 5 C10 5 8 19 20 19" }
            path { d: "M16.6 2.2 20 5 16.6 7.8" }
            path { d: "M16.6 16.2 20 19 16.6 21.8" }
        }
    }
}

#[component]
fn Gear() -> Element {
    rsx! {
        svg {
            width: "18", height: "18", view_box: "0 0 24 24", fill: "none", stroke: "#75798c",
            stroke_width: "1.6",
            circle { cx: "12", cy: "12", r: "3.2" }
            circle { cx: "12", cy: "12", r: "8.4" }
            path { d: "M12 3.6 V6.2 M12 17.8 V20.4 M3.6 12 H6.2 M17.8 12 H20.4" }
        }
    }
}
