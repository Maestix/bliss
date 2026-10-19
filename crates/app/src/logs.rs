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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Lines kept for the log pane. A few hundred is more than anyone scrolls and
/// small enough that the whole buffer is a rounding error in memory.
pub const CAPACITY: usize = 400;

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

    /// A colour per level, all of them softened so a page of log stays calm:
    /// the theme's mint for the ordinary, a muted amber for warnings, the
    /// failure red for errors, and grey for detail.
    pub fn colour(self) -> &'static str {
        match self {
            Self::Error => "#e56b6f",
            Self::Warn => "#d9b16a",
            // --color-good (#72cf9e), a little quieter: it is most of the log.
            Self::Info => "rgba(114,207,158,.8)",
            Self::Debug => "var(--color-neutral-600)",
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

/// Lines recorded since the start, so the pane can tell there is something new.
static REVISION: AtomicU64 = AtomicU64::new(0);

/// A number that changes whenever a line arrives.
///
/// The pane passes it down as a prop. It reads the buffer itself, and a
/// component is re-run by its parent only when its props change, so without
/// this the "live" log drew itself once and then only when clicked.
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn push(line: Line) {
    let mut buffer = buffer().lock().expect("log buffer");
    if buffer.len() == CAPACITY {
        buffer.pop_front();
    }
    buffer.push_back(line);
    REVISION.fetch_add(1, Ordering::Relaxed);
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

/// The most one run of bliss writes into bliss.log.
///
/// The file used to be trimmed when bliss started and never while it ran, so
/// a run could make it as large as the disk: a friend's bliss filled 305 GB
/// with one warning repeated, at some 600 MB/s, before anyone noticed — the
/// TUN stack spinning on an ended device (`Ended` in bliss-tun). An ordinary
/// run writes a few lines an hour at the default level, so this is never met
/// unless something has gone wrong, and then it stops the disk, not the log
/// pane, which keeps its last lines as before.
pub const RUN_LIMIT: u64 = 16 * 1024 * 1024;

/// bliss.log as one run writes it: appended to until the run has written
/// `limit` bytes, then one line saying so, then nothing more.
pub struct LogFile<W> {
    file: W,
    written: u64,
    limit: u64,
    full: bool,
}

impl<W: std::io::Write> LogFile<W> {
    pub fn new(file: W, limit: u64) -> Self {
        Self {
            file,
            written: 0,
            limit,
            full: false,
        }
    }
}

impl<W: std::io::Write> std::io::Write for LogFile<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.full {
            return Ok(buf.len());
        }
        if self.written + buf.len() as u64 > self.limit {
            self.full = true;
            writeln!(
                self.file,
                "--- this run has written {} MB here, the most one run may; what follows is in the window's log only ---",
                self.limit / (1024 * 1024)
            )?;
            return Ok(buf.len());
        }
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

/// Cut the log at `path` down to its last `keep` bytes or less, from the
/// start of a line: the end of the last run is what someone opening it after
/// a problem wants. It used to be deleted whole once past its size, and with
/// it the record of the run that made it grow.
pub fn keep_tail(path: &std::path::Path, keep: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= keep {
        return Ok(());
    }
    file.seek(SeekFrom::Start(len - keep))?;
    let mut tail = Vec::with_capacity(keep as usize);
    file.take(keep).read_to_end(&mut tail)?;
    let start = tail
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(tail.len(), |at| at + 1);
    let trimmed = path.with_extension("log.trim");
    std::fs::write(&trimmed, &tail[start..])?;
    std::fs::rename(&trimmed, path)
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

    /// What wakes the pane. Tests share the buffer, so this asks only that a
    /// line moves the number, not by how much.
    #[test]
    fn every_line_moves_the_revision() {
        let before = revision();
        push(line(1));
        assert!(revision() > before);
    }

    #[test]
    fn history_copes_with_asking_for_more_than_there_is() {
        assert!(history(CAPACITY * 10).len() <= CAPACITY);
    }

    /// A run writes up to its limit, says so once, and then nothing: the
    /// same warning a million times is a few lines and a note, not the disk.
    #[test]
    fn the_file_stops_at_its_limit() {
        use std::io::Write;

        let mut log = LogFile::new(Vec::new(), 100);
        for n in 0..1000 {
            writeln!(log, "warning {n:04} the same again").expect("written");
        }
        let text = String::from_utf8(log.file).expect("text");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "three lines and the note: {text}");
        assert!(lines[3].contains("the most one run may"), "{text}");
        assert!(text.len() < 100 + 200, "{} bytes", text.len());
    }

    /// The trim keeps the newest lines, whole, and no more than asked.
    #[test]
    fn the_trim_keeps_the_last_whole_lines() {
        use std::fmt::Write;

        let dir = std::env::temp_dir().join(format!("bliss-log-trim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let path = dir.join("bliss.log");
        let mut text = String::new();
        for n in 0..1000 {
            writeln!(text, "line {n:04}").expect("formatted");
        }
        std::fs::write(&path, &text).expect("written");

        keep_tail(&path, 100).expect("trimmed");
        let kept = std::fs::read_to_string(&path).expect("read");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(kept.len() <= 100, "{} bytes", kept.len());
        assert!(kept.starts_with("line "), "starts on a line: {kept:?}");
        assert!(
            kept.ends_with("line 0999\n"),
            "ends with the newest: {kept:?}"
        );
    }
}
