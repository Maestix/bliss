//! Compiles `bliss.rc`, which puts the icon inside bliss.exe.
//!
//! Explorer shows an executable's icon only from its own resources, and
//! shortcuts and Start menu entries use that icon unless told otherwise. The
//! window loads the same resource at runtime, so the taskbar shows it too
//! instead of the Dioxus logo.
//!
//! Other systems have no resources: embed-resource answers `NotWindows`,
//! which counts as done, and the window takes its icon from a PNG instead
//! (`app_icon` in ui.rs).

fn main() {
    println!("cargo:rerun-if-changed=bliss.rc");
    println!("cargo:rerun-if-changed=assets/bliss.ico");

    // Required rather than optional. On MSVC a missing rc.exe fails the build
    // either way, but on a GNU toolchain without windres the result is
    // `NotAttempted`, which `manifest_optional` accepts — and an executable
    // without its icon is a broken build of this program that nothing notices.
    if let Err(e) = embed_resource::compile("bliss.rc", embed_resource::NONE).manifest_required() {
        panic!("{e}");
    }
}
