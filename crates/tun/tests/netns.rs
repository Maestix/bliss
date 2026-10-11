//! The tunnel end to end on Linux, in a network namespace of its own.
//!
//! Ignored by default; run with
//!
//! ```text
//! cargo test -p bliss-tun --test netns -- --ignored --nocapture
//! ```
//!
//! The test runs itself again under `unshare --user --map-root-user --net`:
//! a user namespace where it is root, and so holds `CAP_NET_ADMIN`, over a
//! network namespace with nothing in it but loopback. No `sudo`, and nothing
//! on the machine is touched — the device, its routes and the fake uplink
//! all vanish with the namespace. Ubuntu's AppArmor may refuse unprivileged
//! user namespaces on a desktop (`kernel.apparmor_restrict_unprivileged_userns`);
//! WSL does not.
//!
//! Inside: an uplink on a dummy interface with a default route, so there is
//! a gateway to go around the tunnel by; a `Serving` whose outbound answers
//! every connection and datagram with an echo; then the tunnel. A program's
//! connection to an address on the internet goes into the device, comes out
//! of the stack as a connection, is decided, dialled and relayed, and the
//! echo comes back the same way; so does a datagram. The routes are checked
//! in the kernel's own listing, a route a killed bliss left is taken away by
//! the next tunnel, and when the tunnel is dropped the device and every
//! route bliss made are gone. Other programs' routes are never taken over:
//! a tunnel over another VPN's halves of the address space is refused, and
//! an address with a route of its own keeps it.

#![cfg(target_os = "linux")]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bliss_core::{
    BoxedDatagram, BoxedStream, Datagram, EngineInfo, Outbound, Router, Serving, Target, Tracker,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const INSIDE: &str = "BLISS_TUN_NETNS_INSIDE";

/// The address the engine's server stands for: kept around the tunnel.
const SERVER: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 50);
/// Another of its addresses, which a route not bliss's already keeps outside.
const ROUTED: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 51);
/// Somewhere on the internet a program wants to reach.
const SOMEWHERE: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), 7);

#[test]
#[ignore = "runs in a user and network namespace of its own; see the file's head"]
fn the_tunnel_carries_a_program_in_a_namespace_of_its_own() {
    if std::env::var_os(INSIDE).is_some() {
        return inside();
    }
    let exe = std::env::current_exe().expect("the test's own path");
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(exe)
        .args([
            "--exact",
            "the_tunnel_carries_a_program_in_a_namespace_of_its_own",
            "--ignored",
            "--nocapture",
        ])
        .env(INSIDE, "1")
        .status()
        .expect("unshare runs (util-linux)");
    assert!(status.success(), "the namespace run failed: {status}");
}

fn ip(args: &str) -> String {
    let out = Command::new("ip")
        .args(args.split_whitespace())
        .output()
        .expect("ip runs");
    assert!(
        out.status.success(),
        "ip {args}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn inside() {
    ip("link set lo up");
    ip("link add up0 type dummy");
    ip("addr add 192.0.2.2/24 dev up0");
    ip("link set up0 up");
    ip("route add default via 192.0.2.1 dev up0");
    assert!(
        bliss_tun::elevated(),
        "root in the namespace has CAP_NET_ADMIN"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    // Another VPN already taking the whole machine, with halves of its own.
    ip("link add vpn0 type dummy");
    ip("addr add 10.8.0.2/24 dev vpn0");
    ip("link set vpn0 up");
    ip("route add 0.0.0.0/1 dev vpn0");
    ip("route add 128.0.0.0/1 dev vpn0");
    runtime.block_on(refused());
    ip("link del vpn0");

    // What a killed bliss would have left: a route of its own around the
    // tunnel, which the next tunnel has to take away.
    ip("route add 198.51.100.1 via 192.0.2.1 dev up0 proto 98");
    // And a route someone else made to one of the server's addresses.
    ip(&format!("route add {ROUTED} via 192.0.2.1 dev up0"));
    runtime.block_on(carried());

    // The tunnel is gone: no device, and none of bliss's routes.
    assert!(
        !ip("-o link show").contains(" bliss:"),
        "the device outlived the tunnel"
    );
    let left = ip("-4 route show proto 98");
    assert!(left.trim().is_empty(), "routes outlived the tunnel: {left}");
    let theirs = ip(&format!("-4 route show {ROUTED}"));
    assert!(
        theirs.contains("via 192.0.2.1 dev up0"),
        "someone else's route outlives the tunnel: {theirs:?}"
    );
}

fn serving() -> (Arc<Tracker>, Arc<Serving>) {
    let tracker = Arc::new(Tracker::new());
    let serving = Arc::new(Serving::new(
        Arc::new(Echo),
        Arc::clone(&tracker),
        Arc::new(Router::proxy_everything()),
    ));
    (tracker, serving)
}

fn work_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("bliss-tun-netns")
}

/// A tunnel over another's halves of the address space is refused, saying
/// whose they are, and theirs stay as they were: replaced, they would have
/// been deleted with bliss's own when the tunnel stopped.
async fn refused() {
    let (_, serving) = serving();
    let error = match bliss_tun::Tun::up(&work_dir(), serving, &[SERVER]) {
        Ok(_) => panic!("a tunnel over another VPN's routes"),
        Err(e) => e,
    };
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
    assert!(
        error.to_string().contains("vpn0"),
        "names what is in the way: {error}"
    );
    let theirs = ip("-4 route show dev vpn0");
    assert!(
        theirs.contains("0.0.0.0/1") && theirs.contains("128.0.0.0/1"),
        "the other tunnel keeps its routes: {theirs}"
    );
    let left = ip("-4 route show proto 98");
    assert!(left.trim().is_empty(), "nothing of bliss's is left: {left}");
    assert!(!ip("-o link show").contains(" bliss:"), "nor its device");
}

async fn carried() {
    let (tracker, serving) = serving();
    let tun = bliss_tun::Tun::up(&work_dir(), Arc::clone(&serving), &[SERVER, ROUTED])
        .expect("the tunnel");

    let routes = ip("-4 route show proto 98");
    assert!(routes.contains("0.0.0.0/1 dev bliss"), "{routes}");
    assert!(routes.contains("128.0.0.0/1 dev bliss"), "{routes}");
    assert!(
        routes.contains("203.0.113.50 via 192.0.2.1 dev up0"),
        "the server goes around: {routes}"
    );
    assert!(
        !routes.contains(&ROUTED.to_string()),
        "an address with a route of its own keeps it: {routes}"
    );
    assert!(
        !routes.contains("198.51.100.1"),
        "a route a previous run left is taken away: {routes}"
    );
    let way = ip(&format!("-4 route get {}", SOMEWHERE.ip()));
    assert!(
        way.contains("dev bliss"),
        "the internet goes into the tunnel: {way}"
    );

    // A program's connection, through the device and back.
    let wait = Duration::from_secs(10);
    let mut program = tokio::time::timeout(wait, tokio::net::TcpStream::connect(SOMEWHERE))
        .await
        .expect("connects in time")
        .expect("connects");
    let sent = b"through the tunnel and back";
    program.write_all(sent).await.expect("writes");
    let mut echoed = vec![0u8; sent.len()];
    tokio::time::timeout(wait, program.read_exact(&mut echoed))
        .await
        .expect("answers in time")
        .expect("reads");
    assert_eq!(&echoed, sent);
    let table = tracker.table();
    assert!(
        table.iter().any(|row| row.target == SOMEWHERE.to_string()),
        "the connection is in the table"
    );
    drop(program);

    // And a datagram.
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("binds");
    let to = SocketAddr::new(SOMEWHERE.ip(), 53);
    socket.send_to(b"a question", to).await.expect("sends");
    let mut answer = [0u8; 64];
    let (n, from) = tokio::time::timeout(wait, socket.recv_from(&mut answer))
        .await
        .expect("answers in time")
        .expect("receives");
    assert_eq!(&answer[..n], b"a question");
    assert_eq!(from, to, "the answer comes from where it was asked");

    drop(tun);
    // The stack lets the device go as its task ends.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// An engine that echoes: every connection and every datagram comes back.
struct Echo;

#[async_trait]
impl Outbound for Echo {
    async fn connect_tcp(&self, _target: &Target) -> io::Result<BoxedStream> {
        let (near, far) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let (mut read, mut write) = tokio::io::split(far);
            let _ = tokio::io::copy(&mut read, &mut write).await;
        });
        Ok(Box::new(near))
    }

    async fn bind_udp(&self, _target: &Target) -> io::Result<BoxedDatagram> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        Ok(Box::new(EchoDatagram {
            tx,
            rx: tokio::sync::Mutex::new(rx),
        }))
    }

    fn describe(&self) -> EngineInfo {
        EngineInfo {
            kind: "echo",
            version: String::new(),
            external: false,
        }
    }
}

struct EchoDatagram {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
}

#[async_trait]
impl Datagram for EchoDatagram {
    async fn send(&self, payload: &[u8]) -> io::Result<()> {
        self.tx
            .send(payload.to_vec())
            .await
            .map_err(|_| io::Error::other("closed"))
    }

    async fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(payload) = self.rx.lock().await.recv().await else {
            return Ok(0);
        };
        let n = payload.len().min(buf.len());
        buf[..n].copy_from_slice(&payload[..n]);
        Ok(n)
    }
}
