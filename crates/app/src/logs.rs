//! The log, as the window shows it.
//!
//! A `tracing` layer that keeps the last few hundred lines in memory so the UI
//! can render them, alongside the usual stdout output. Everything bliss logs
//! passes through here, including xray's own output, which the process
//! supervisor re-emits with an `xray:` prefix.
//!
//! The cap is the point. An unbounded log is one of the two ways a client of
//! this kind reaches gigabytes — the other is the connection table — so the
//! buffer drops the oldest line rather than growing, and a test holds it to it.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Lines kept for the log pane. A few hundred is more than anyone scrolls and
/// small enough that the whole buffer is a rounding error in memory.
const CAPACITY: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub time: String,
    pub level: Severity,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warn,
    Info,
    Debug,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// Colours from the Nocturne ramp: only trouble gets to stand out.
    pub fn colour(self) -> &'static str {
        match self {
            Self::Error => "#a8505c",
            Self::Warn => "#d2cefd",
            Self::Info => "var(--color-neutral-500)",
            Self::Debug => "var(--color-neutral-700)",
        }
    }
}

impl From<&Level> for Severity {
    fn from(level: &Level) -> Self {
        match *level {
            Level::ERROR => Self::Error,
            Level::WARN => Self::Warn,
            Level::INFO => Self::Info,
            _ => Self::Debug,
        }
    }
}

static BUFFER: OnceLock<Mutex<VecDeque<Line>>> = OnceLock::new();

fn buffer() -> &'static Mutex<VecDeque<Line>> {
    BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// The buffer newest-first, capped at `limit`.
///
/// Newest first on purpose: without JavaScript there is no way to pin a pane to
/// its bottom edge, so the alternative was a log whose latest line was always
/// just out of sight. Reading downwards into the past is how most log viewers
/// work anyway.
pub fn history(limit: usize) -> Vec<Line> {
    let buffer = buffer().lock().expect("log buffer");
    buffer.iter().rev().take(limit).cloned().collect()
}

fn push(line: Line) {
    let mut buffer = buffer().lock().expect("log buffer");
    if buffer.len() == CAPACITY {
        buffer.pop_front();
    }
    buffer.push_back(line);
}

/// Feeds the in-memory buffer. Install alongside the usual fmt layer.
pub struct Recorder;

impl<S: Subscriber> Layer<S> for Recorder {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut message = Message::default();
        event.record(&mut message);
        if message.0.is_empty() {
            return;
        }
        push(Line {
            time: jiff::Zoned::now().strftime("%H:%M:%S").to_string(),
            level: event.metadata().level().into(),
            text: message.0,
        });
    }
}

/// Pulls just the `message` field out of an event; the rest is structure the
/// pane has no room for.
#[derive(Default)]
struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
            // tracing renders the message through Debug, which wraps a plain
            // string in quotes. The pane wants the string.
            if self.0.len() >= 2 && self.0.starts_with('"') && self.0.ends_with('"') {
                self.0 = self.0[1..self.0.len() - 1].replace("\\\"", "\"");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> Line {
        Line {
            time: "00:00:00".into(),
            level: Severity::Info,
            text: format!("line {n}"),
        }
    }

    /// The rule this module exists for.
    #[test]
    fn the_buffer_cannot_grow_without_bound() {
        for n in 0..CAPACITY * 3 {
            push(line(n));
        }
        assert_eq!(buffer().lock().unwrap().len(), CAPACITY);

        let recent = history(5);
        assert_eq!(recent.len(), 5);
        // Newest first, and what survived is the newest end.
        assert_eq!(recent[0].text, format!("line {}", CAPACITY * 3 - 1));
        assert_eq!(recent[4].text, format!("line {}", CAPACITY * 3 - 5));
    }

    #[test]
    fn history_copes_with_asking_for_more_than_there_is() {
        assert!(history(CAPACITY * 10).len() <= CAPACITY);
    }
}
