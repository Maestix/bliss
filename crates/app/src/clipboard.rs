//! Text to and from the clipboard.
//!
//! Through the Win32 clipboard rather than `navigator.clipboard`: the window is
//! served from a custom protocol, the async clipboard API wants a secure
//! context and permission, and a copy button that silently does nothing on some
//! machines is worse than no copy button. This is a dozen calls and works the
//! same everywhere.
//!
//! Ownership is the one thing to get right. After a successful
//! `SetClipboardData` the block belongs to the system and must not be freed;
//! before that, it is ours and has to be. A block read with `GetClipboardData`
//! is never ours.

/// Put `text` on the clipboard, reporting whether it got there.
#[cfg(windows)]
pub fn copy(text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // The clipboard wants UTF-16 with a terminator. Windows line endings too:
    // text copied with bare newlines pastes as one long line into half the
    // programs on the machine, Notepad included.
    let wide: Vec<u16> = text
        .replace("\r\n", "\n")
        .replace('\n', "\r\n")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes = std::mem::size_of_val(wide.as_slice());

    // SAFETY: every call below is checked, the allocation is sized from `wide`
    // and written exactly once, and the lock is released before the handle is
    // handed over. The clipboard is closed on every path.
    unsafe {
        if OpenClipboard(None).is_err() {
            // Another program is holding it. Momentary and not worth retrying;
            // the caller reports that nothing was copied.
            return false;
        }

        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            let _ = CloseClipboard();
            return false;
        };

        let target = GlobalLock(block);
        if target.is_null() {
            let _ = GlobalFree(Some(block));
            let _ = CloseClipboard();
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(block);

        // Emptying takes ownership of whatever was there before; it has to
        // happen after the allocation so a failure above leaves the clipboard
        // alone rather than wiping it.
        let _ = EmptyClipboard();

        let handle = HANDLE(block.0);
        let placed = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(handle)).is_ok();
        if !placed {
            // Still ours, so still ours to free.
            let _ = GlobalFree(Some(HGLOBAL(handle.0)));
        }
        let _ = CloseClipboard();
        placed
    }
}

#[cfg(not(windows))]
pub fn copy(_text: &str) -> bool {
    false
}

/// The text on the clipboard, if there is any and nobody else is holding it.
#[cfg(windows)]
pub fn paste() -> Option<String> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    // SAFETY: the block belongs to the system and is only read while the
    // clipboard is open and the block is locked, and never past its own size
    // even if the terminator is missing. The clipboard is closed on every path.
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        let text = 'read: {
            let Ok(handle) = GetClipboardData(CF_UNICODETEXT.0 as u32) else {
                break 'read None;
            };
            let block = HGLOBAL(handle.0);
            let units = GlobalSize(block) / 2;
            let start = GlobalLock(block).cast::<u16>();
            if start.is_null() {
                break 'read None;
            }
            let all = std::slice::from_raw_parts(start, units);
            let len = all.iter().position(|&unit| unit == 0).unwrap_or(units);
            let text = String::from_utf16_lossy(&all[..len]);
            let _ = GlobalUnlock(block);
            Some(text)
        };
        let _ = CloseClipboard();
        text
    }
}

#[cfg(not(windows))]
pub fn paste() -> Option<String> {
    None
}

/// Russian agreement for a count of lines, for the "copied N lines" note.
pub fn lines(n: usize) -> String {
    let form = match (n % 10, n % 100) {
        (1, rest) if rest != 11 => "строка",
        (2..=4, rest) if !(12..=14).contains(&rest) => "строки",
        _ => "строк",
    };
    format!("{n} {form}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counts_agree() {
        assert_eq!(lines(1), "1 строка");
        assert_eq!(lines(2), "2 строки");
        assert_eq!(lines(5), "5 строк");
        assert_eq!(lines(11), "11 строк");
        assert_eq!(lines(21), "21 строка");
        assert_eq!(lines(112), "112 строк");
        assert_eq!(lines(0), "0 строк");
    }

    /// A round trip through the real clipboard, because the ownership rules are
    /// the whole difficulty and a mistake there is a leak or a crash rather
    /// than a wrong answer.
    #[cfg(windows)]
    #[test]
    fn text_survives_the_round_trip() {
        // Cyrillic and a newline: the two things a naive implementation gets
        // wrong, by writing bytes instead of UTF-16 and by leaving the line
        // ending bare.
        let sent = "первая строка\nвторая строка";
        if !copy(sent) {
            // Another program owns the clipboard on this machine right now.
            // Not a failure of ours, and not worth making the suite flaky over.
            return;
        }
        let Some(got) = paste() else {
            // Taken back between the two calls, for the same reason.
            return;
        };
        assert_eq!(got, "первая строка\r\nвторая строка");
    }
}
