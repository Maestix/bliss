//! Shortening for values that must stay recognisable but must not be readable.
//!
//! A ServerConfig is printed into the log pane, into `bliss check` and into
//! whatever a user pastes when reporting a problem. Anything derived from the
//! link is credential material, so `Debug` shows just enough of it to tell two
//! servers apart and no more.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Keep the first 8 and last 4 characters: `vByYYHWa…AkSY`.
///
/// Short inputs are returned whole — there is nothing to hide in a value that
/// is already too small to be a key, and an ellipsis would only mislead.
pub(crate) fn head_tail(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 13 {
        return s.to_owned();
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    head_tail(&URL_SAFE_NO_PAD.encode(bytes))
}
