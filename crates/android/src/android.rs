//! The calls the Kotlin side makes into the engine (`app.bliss.Native`).
//!
//! Three, and nothing else crosses JNI: everything the screens need goes over
//! the engine's socket instead (engine.rs), so the screens' process never
//! loads this library at all.

use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use std::sync::atomic::{AtomicU64, Ordering};

use jni::sys::{jboolean, jint, jlong};

use crate::engine::{self, Cmd, Paths};

fn string(env: &mut JNIEnv, value: &JString) -> Option<String> {
    env.get_string(value).ok().map(Into::into)
}

/// Start the engine: the app's folder, the native libraries' folder (where
/// xray is), and the socket's path. Answers whether it is running.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_bliss_Native_start(
    mut env: JNIEnv,
    _class: JClass,
    files: JString,
    libs: JString,
    socket: JString,
) -> jboolean {
    let (Some(files), Some(libs), Some(socket)) = (
        string(&mut env, &files),
        string(&mut env, &libs),
        string(&mut env, &socket),
    ) else {
        return 0;
    };
    let paths = Paths {
        files: PathBuf::from(files),
        libs: PathBuf::from(libs),
        socket: PathBuf::from(socket),
    };
    match engine::start(paths) {
        Ok(_) => 1,
        Err(e) => {
            eprintln!("bliss: the engine did not start: {e}");
            0
        }
    }
}

static TICKETS: AtomicU64 = AtomicU64::new(0);

/// The service established the TUN: hand its descriptor over. The engine
/// owns it from here, and closes it when the tunnel ends. Answers the ticket
/// the engine's frames will name it by (`Frame::ticket`), or 0 when the
/// engine is not running.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_bliss_Native_connect(
    _env: JNIEnv,
    _class: JClass,
    fd: jint,
) -> jlong {
    if fd < 0 {
        return 0;
    }
    // SAFETY: the Kotlin side detached this descriptor from its
    // ParcelFileDescriptor, so nothing else will close it.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let ticket = TICKETS.fetch_add(1, Ordering::SeqCst) + 1;
    match engine::handle() {
        Some(handle) if handle.commands.send(Cmd::Connect(fd, ticket)).is_ok() => ticket as jlong,
        _ => 0,
    }
}

/// Take the tunnel down, from the notification's button or the tile.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_bliss_Native_disconnect(_env: JNIEnv, _class: JClass) {
    if let Some(handle) = engine::handle() {
        let _ = handle
            .commands
            .send(Cmd::Ui(crate::protocol::UiCommand::Disconnect));
    }
}
