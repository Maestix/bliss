//! libxdo, kept from being a library bliss cannot start without.
//!
//! muda, the menus under dioxus-desktop, links libxdo for one thing: its
//! Copy, Cut, Paste and Select All items type their own shortcut through it.
//! bliss shows none of those items. The link alone still made `libxdo.so.3`
//! a library the binary would not start without, and Arch's xdotool ships
//! `libxdo.so.4`, so a build made on Ubuntu could not start on Arch at all.
//! dioxus-desktop takes muda with its default features, so the link cannot be
//! switched off from here.
//!
//! Defining the three functions muda calls in the binary itself leaves the
//! library nothing to give, and the linker drops it (`--as-needed`, which
//! rustc passes and Ubuntu's gcc assumes). Should such an item ever appear, it
//! would do nothing: `xdo_new` answers with no handle, which muda already
//! takes as "no xdo here".

use std::ffi::{c_char, c_int, c_ulong, c_void};

/// No handle: muda's `XDo::new` reads a null as failure and types nothing.
#[unsafe(no_mangle)]
pub extern "C" fn xdo_new(_display: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn xdo_send_keysequence_window(
    _xdo: *const c_void,
    _window: c_ulong,
    _keysequence: *const c_char,
    _delay: u32,
) -> c_int {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn xdo_free(_xdo: *mut c_void) {}
