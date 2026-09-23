//! Inline SVG icons.
//!
//! Colour is an attribute, never CSS. Blitz renders SVG but does not style it,
//! so `currentColor` or a class-driven fill would work under WebView2 and
//! silently vanish under the other renderer — found in the 1.0 spike, and cheap
//! to keep right. Every icon therefore takes the colour it should be drawn in.

use dioxus::prelude::*;

/// The stroke width the whole set is drawn at, so icons look like one family.
const STROKE: &str = "1.6";

#[component]
pub fn Power(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: "1.7", stroke_linecap: "round",
            path { d: "M12 3.2 V12.4" }
            path { d: "M7.4 6.6 A7.6 7.6 0 1 0 16.6 6.6" }
        }
    }
}

/// The switch on the status card, in place of the power sign: filled once the
/// tunnel is up, an outline while it is not, and the colour is the state's.
///
/// Its own path rather than one lifted from an icon set, drawn in this family's
/// terms: two arcs for the lobes, a cubic down each side to the point.
#[component]
pub fn Heart(size: u32, color: String, filled: bool) -> Element {
    let fill = if filled {
        color.clone()
    } else {
        "none".to_owned()
    };
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "{fill}",
            stroke: "{color}", stroke_width: "1.7", stroke_linejoin: "round",
            stroke_linecap: "round",
            path {
                d: "M12 20.4 C10.4 19 4.2 14.9 4.2 10.4 A4.4 4.4 0 0 1 12 7.8 \
                    A4.4 4.4 0 0 1 19.8 10.4 C19.8 14.9 13.6 19 12 20.4 Z"
            }
        }
    }
}

#[component]
pub fn Shield(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: "1.7", stroke_linejoin: "round",
            path { d: "M12 2.6 20 5.6 V12 c0 5-3.6 8-8 9.4 C8 20 4.4 17 4.4 12 V5.6 Z" }
            path { d: "M8.6 11.8 11.2 14.4 15.6 9.6", stroke_linecap: "round" }
        }
    }
}

/// One side of the TUN switch: only what is plugged into bliss goes through
/// it — the programs pointed at its two ports.
#[component]
pub fn Plug(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M9 3 V7.6" }
            path { d: "M15 3 V7.6" }
            path { d: "M6.6 7.6 h10.8 v3 a5.4 5.4 0 0 1-10.8 0 Z" }
            path { d: "M12 16 V21" }
        }
    }
}

/// The other side: the whole machine's traffic, which is what a TUN adapter
/// takes.
#[component]
pub fn Machine(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M6.2 5.4 h11.6 a1.4 1.4 0 0 1 1.4 1.4 v8 h-14.4 v-8 a1.4 1.4 0 0 1 1.4-1.4 Z" }
            path { d: "M3 18.6 h18" }
        }
    }
}

#[component]
pub fn Globe(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE,
            circle { cx: "12", cy: "12", r: "8.6" }
            path { d: "M3.4 12 H20.6" }
            path { d: "M12 3.4 a13 13 0 0 1 0 17.2 a13 13 0 0 1 0-17.2" }
        }
    }
}

/// The broadcast arcs used for subscriptions.
#[component]
pub fn Rss(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            path { d: "M5 18.6 h.01" }
            path { d: "M5 12.4 a6.2 6.2 0 0 1 6.2 6.2" }
            path { d: "M5 6.2 a12.4 12.4 0 0 1 12.4 12.4" }
        }
    }
}

/// Two paths crossing: traffic taking one route or the other.
#[component]
pub fn Split(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            path { d: "M4 19 C10 19 8 5 20 5" }
            path { d: "M4 5 C10 5 8 19 20 19" }
            path { d: "M16.6 2.2 20 5 16.6 7.8" }
            path { d: "M16.6 16.2 20 19 16.6 21.8" }
        }
    }
}

#[component]
pub fn Terminal(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            rect {
                x: "3.2", y: "4.4", width: "17.6", height: "15.2", rx: "2.4",
                stroke_linejoin: "round",
            }
            path { d: "M7.4 9.6 10.4 12 7.4 14.4" }
            path { d: "M12.6 15 H16.4" }
        }
    }
}

#[component]
pub fn Gear(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE,
            circle { cx: "12", cy: "12", r: "3.2" }
            circle { cx: "12", cy: "12", r: "8.4" }
            path { d: "M12 3.6 V6.2 M12 17.8 V20.4 M3.6 12 H6.2 M17.8 12 H20.4" }
        }
    }
}

#[component]
pub fn Plus(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            path { d: "M12 5.4 V18.6 M5.4 12 H18.6" }
        }
    }
}

/// A pulse line, for "check every server".
#[component]
pub fn Pulse(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M2.8 12 H7.2 L9.6 5.6 14.4 18.4 16.8 12 H21.2" }
        }
    }
}

/// A circular arrow, for restarting something that is already running.
#[component]
pub fn Refresh(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M20 12 A8 8 0 1 1 17.4 6.1" }
            path { d: "M20.4 3.4 V7.4 H16.4" }
        }
    }
}

#[component]
pub fn Folder(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linejoin: "round",
            path { d: "M3.4 6.6 a1.8 1.8 0 0 1 1.8-1.8 H9.4 l2 2.6 H18.8 a1.8 1.8 0 0 1 1.8 1.8 V17.4 a1.8 1.8 0 0 1-1.8 1.8 H5.2 a1.8 1.8 0 0 1-1.8-1.8 Z" }
        }
    }
}

#[component]
pub fn Copy(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linejoin: "round",
            rect { x: "8.6", y: "8.6", width: "11.8", height: "11.8", rx: "2.2" }
            path { d: "M15.4 5.4 H5.8 a2.4 2.4 0 0 0-2.4 2.4 V17" }
        }
    }
}

/// Moving a rule earlier.
#[component]
pub fn ChevronUp(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M6.6 14.8 12 9.4 17.4 14.8" }
        }
    }
}

/// Moving a rule later.
#[component]
pub fn ChevronDown(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M6.6 9.2 12 14.6 17.4 9.2" }
        }
    }
}

/// A closed group, which a click opens.
#[component]
pub fn ChevronRight(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M9.2 6.6 14.6 12 9.2 17.4" }
        }
    }
}

#[component]
pub fn Pencil(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M15.2 4.8 19.2 8.8 8.6 19.4 4 20 4.6 15.4 Z" }
            path { d: "M13.2 6.8 17.2 10.8" }
        }
    }
}

#[component]
pub fn Trash(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M4.6 6.8 H19.4" }
            path { d: "M9.4 6.8 V4.8 a1 1 0 0 1 1-1 H13.6 a1 1 0 0 1 1 1 V6.8" }
            path { d: "M6.4 6.8 7.4 19 a1.6 1.6 0 0 0 1.6 1.4 H15 a1.6 1.6 0 0 0 1.6-1.4 L17.6 6.8" }
        }
    }
}

#[component]
pub fn Search(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: STROKE, stroke_linecap: "round",
            circle { cx: "10.6", cy: "10.6", r: "6.2" }
            path { d: "M15.2 15.2 20 20" }
        }
    }
}

#[component]
pub fn Check(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 24 24", fill: "none",
            stroke: "{color}", stroke_width: "1.8", stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M5.4 12.6 9.8 17 18.6 7.6" }
        }
    }
}

// --- window buttons -------------------------------------------------------
//
// Drawn rather than typed. The Unicode glyphs for these (–, □, ×) come out thin
// and small at any size that fits the bar, because they are letters sitting on a
// baseline, not symbols centred in a box.

#[component]
pub fn Minimise(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 16 16", fill: "none",
            stroke: "{color}", stroke_width: "1.3", stroke_linecap: "round",
            path { d: "M3.2 8 H12.8" }
        }
    }
}

#[component]
pub fn Maximise(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 16 16", fill: "none",
            stroke: "{color}", stroke_width: "1.3",
            rect { x: "3.4", y: "3.4", width: "9.2", height: "9.2", rx: "1.4" }
        }
    }
}

#[component]
pub fn Close(size: u32, color: String) -> Element {
    rsx! {
        svg {
            width: "{size}", height: "{size}", view_box: "0 0 16 16", fill: "none",
            stroke: "{color}", stroke_width: "1.3", stroke_linecap: "round",
            path { d: "M4 4 L12 12 M12 4 L4 12" }
        }
    }
}
