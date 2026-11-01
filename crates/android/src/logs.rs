//! The log: the last lines in memory for the journal tab, and every line in
//! logcat. Nothing is written to a file — a phone's flash wears, and the
//! desktop's 305 GB log (AGENTS.md §5) is reason enough to hold a ring and no
//! more. The ring has a hard cap and a test that fails without it.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::Mutex;

/// The journal tab shows at most this many lines.
pub const CAP: usize = 400;

static RING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Keep one line, dropping the oldest past the cap.
pub fn push(line: String) {
    let mut ring = RING.lock().expect("log ring");
    if ring.len() == CAP {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// The lines held, oldest first.
pub fn lines() -> Vec<String> {
    RING.lock().expect("log ring").iter().cloned().collect()
}

/// Start logging. Called once, from the first call into the library.
pub fn init() {
    let filter = tracing_subscriber::EnvFilter::new("info,ipstack=error");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(|| Line(Vec::new()))
        .with_ansi(false)
        .with_target(false)
        .with_level(true)
        .without_time()
        .try_init();
}

/// One formatted line on its way: into the ring when it is finished.
struct Line(Vec<u8>);

impl Write for Line {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        let text = String::from_utf8_lossy(&self.0).trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        logcat(&text);
        let time = jiff::Zoned::now().strftime("%H:%M:%S").to_string();
        push(format!("{time} {text}"));
    }
}

#[cfg(target_os = "android")]
fn logcat(text: &str) {
    use std::ffi::CString;
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(
            prio: i32,
            tag: *const libc::c_char,
            text: *const libc::c_char,
        ) -> i32;
    }
    let (Ok(tag), Ok(text)) = (CString::new("bliss"), CString::new(text.replace('\0', " "))) else {
        return;
    };
    // SAFETY: two NUL-terminated strings that outlive the call; 4 is INFO.
    unsafe {
        __android_log_write(4, tag.as_ptr(), text.as_ptr());
    }
}

#[cfg(not(target_os = "android"))]
fn logcat(_text: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_stops_at_its_cap() {
        for n in 0..CAP + 50 {
            push(format!("line {n}"));
        }
        let held = lines();
        assert_eq!(held.len(), CAP);
        assert_eq!(held.last().map(String::as_str), Some("line 449"));
    }
}
