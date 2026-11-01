//! The calls the Kotlin side makes into the engine (`app.bliss.Native`).
//!
//! Three, and one call back: everything the screens need goes over the
//! engine's socket instead (engine.rs), so the screens' process never loads
//! this library at all. The call back is `Native.ownerOf`, which asks Android
//! whose a connection is (`owner_of`).

use std::net::SocketAddr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
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
    class: JClass,
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
    // Kept for the call back: a thread the engine made finds no app class by
    // name, so the class is held from here, where the app called in.
    if let (Ok(vm), Ok(class)) = (env.get_java_vm(), env.new_global_ref(&class)) {
        let _ = JAVA.set((vm, class));
    }
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

static JAVA: OnceLock<(JavaVM, GlobalRef)> = OnceLock::new();

/// The package behind a connection through the TUN, asked of Android
/// (`ConnectivityManager.getConnectionOwnerUid`, which answers only the
/// VPN's own app — the service). Blocking: a call into the system.
pub fn owner_of(tcp: bool, local: SocketAddr, remote: SocketAddr) -> Option<String> {
    let (vm, class) = JAVA.get()?;
    // Attached for this call and detached after: the threads asking come
    // and go, and one that ended attached would take the process with it.
    let mut env = vm.attach_current_thread().ok()?;
    let local_ip = env.new_string(local.ip().to_string()).ok()?;
    let remote_ip = env.new_string(remote.ip().to_string()).ok()?;
    let class: &JClass = class.as_obj().into();
    let answer = env.call_static_method(
        class,
        "ownerOf",
        "(ZLjava/lang/String;ILjava/lang/String;I)Ljava/lang/String;",
        &[
            JValue::Bool(u8::from(tcp)),
            JValue::Object(&local_ip),
            JValue::Int(i32::from(local.port())),
            JValue::Object(&remote_ip),
            JValue::Int(i32::from(remote.port())),
        ],
    );
    let object: JObject = match answer.and_then(|value| value.l()) {
        Ok(object) => object,
        Err(_) => {
            let _ = env.exception_clear();
            return None;
        }
    };
    if object.is_null() {
        return None;
    }
    let name: String = env.get_string(&JString::from(object)).ok()?.into();
    Some(name)
}
