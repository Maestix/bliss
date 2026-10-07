//! The work VPN carried end to end by the real OpenVPN: a server and a
//! client on this machine, both started through OpenVPN's interactive
//! service, the client by `bliss_openvpn::Session` from a profile of the
//! shape a work network hands out; behind the server, on its side of the
//! tunnel, a TCP echo and a DNS server, reached through `bliss_core::Through`
//! the way a rule's connections reach the work network.
//!
//! Ignored by default. It needs OpenVPN Community with its service running,
//! an account the service takes any options from (Administrators or
//! "OpenVPN Administrators"), and OpenSSL for the certificates:
//!
//! ```text
//! cargo test -p bliss-openvpn --test service -- --ignored --nocapture
//! ```
//!
//! `BLISS_OPENVPN` names `openvpn.exe` and `BLISS_OPENSSL` `openssl.exe`;
//! otherwise OpenVPN's own folder, then Git's, then the PATH. Every key and
//! certificate is made for the run and thrown away with its folder.
//!
//! What it holds bliss to: OpenVPN calls back and comes up; the profile with
//! its key leaves the disk once read; what the server pushes —
//! `redirect-gateway`, a route, a DNS server — never reaches the machine,
//! while bliss's own route does and the DNS server is reported to bliss
//! instead; a datagram, a connection and a name go through the tunnel and
//! their answers come back by it, by the client adapter's own byte counts;
//! asked to stop, OpenVPN goes and takes its address and route with it; and
//! when the side bliss holds closes, as it does when bliss dies, OpenVPN
//! follows.
//!
//! Its first run met the bug a friend's machine had shown as "OpenVPN не
//! вышел на связь за 20 с": the call came within 25ms and bliss turned it
//! away, since the service's access list hid OpenVPN's name from the right
//! bliss asked for it with (`session::answer`). A server from that run lived
//! on for the same reason in `service::terminate`, and a TAP adapter keeps an
//! address it was ever given by DHCP, so the next runs found the server's
//! address on two adapters and lost every TCP answer between them: the
//! server is told `ip-win32 netsh`, which the service undoes, and is found by
//! the adapter its log names.

#![cfg(windows)]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bliss_core::{EngineInfo, Outbound, Target, Through};
use bliss_openvpn::{Event, Session, service};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The VPN's network, unlikely to be anyone's real one.
const SERVER: Ipv4Addr = Ipv4Addr::new(10, 231, 89, 1);
/// A name only the VPN's DNS server knows.
const INTRANET: &str = "intranet.example";

fn openvpn() -> PathBuf {
    std::env::var_os("BLISS_OPENVPN").map_or_else(
        || PathBuf::from(r"C:\Program Files\OpenVPN\bin\openvpn.exe"),
        PathBuf::from,
    )
}

fn openssl() -> PathBuf {
    if let Some(path) = std::env::var_os("BLISS_OPENSSL") {
        return path.into();
    }
    [
        r"C:\Program Files\OpenVPN\bin\openssl.exe",
        r"C:\Program Files\Git\mingw64\bin\openssl.exe",
        r"C:\Program Files\Git\usr\bin\openssl.exe",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
    .unwrap_or_else(|| PathBuf::from("openssl"))
}

const OPENSSL_CNF: &str = "\
[req]
distinguished_name = dn
[dn]
[ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
[server]
basicConstraints = CA:FALSE
keyUsage = critical,digitalSignature,keyAgreement
extendedKeyUsage = serverAuth
[client]
basicConstraints = CA:FALSE
keyUsage = critical,digitalSignature,keyAgreement
extendedKeyUsage = clientAuth
";

/// A folder with a CA, a certificate for each side and a `tls-crypt` key,
/// deleted when dropped.
struct Kit {
    dir: PathBuf,
}

impl Kit {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("bliss-openvpn-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder for the run");
        std::fs::write(dir.join("openssl.cnf"), OPENSSL_CNF).expect("openssl.cnf");
        let kit = Self { dir };
        let curve = [
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
        ];
        kit.openssl(
            &[
                &[
                    "req",
                    "-x509",
                    "-config",
                    "openssl.cnf",
                    "-extensions",
                    "ca",
                ][..],
                &curve,
                &["-keyout", "ca.key", "-out", "ca.crt", "-days", "2"],
                &["-subj", "/CN=bliss-test-ca"],
            ]
            .concat(),
        );
        for (side, serial) in [("server", "2"), ("client", "3")] {
            let subject = format!("/CN=bliss-test-{side}");
            let (key, request, cert) = (
                format!("{side}.key"),
                format!("{side}.csr"),
                format!("{side}.crt"),
            );
            kit.openssl(
                &[
                    &["req", "-new", "-config", "openssl.cnf"][..],
                    &curve,
                    &["-keyout", &key, "-out", &request, "-subj", &subject],
                ]
                .concat(),
            );
            kit.openssl(&[
                "x509",
                "-req",
                "-in",
                &request,
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-set_serial",
                serial,
                "-days",
                "2",
                "-extfile",
                "openssl.cnf",
                "-extensions",
                side,
                "-out",
                &cert,
            ]);
        }
        let made = Command::new(openvpn())
            .current_dir(&kit.dir)
            .args(["--genkey", "tls-crypt", "tc.key"])
            .output()
            .expect("openvpn.exe runs");
        assert!(
            made.status.success(),
            "openvpn --genkey: {}",
            String::from_utf8_lossy(&made.stderr)
        );
        kit
    }

    fn openssl(&self, args: &[&str]) {
        let out = Command::new(openssl())
            .current_dir(&self.dir)
            .env("OPENSSL_CONF", self.dir.join("openssl.cnf"))
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("openssl ({}): {e}", openssl().display()));
        assert!(
            out.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    /// The server's profile, with everything a work server might push that
    /// bliss must keep off the machine.
    fn server(&self, port: u16) -> PathBuf {
        let path = self.dir.join("server.ovpn");
        let profile = format!(
            "server {network} 255.255.255.0
topology subnet
dev tun
disable-dco
ip-win32 netsh
proto udp4
local 127.0.0.1
port {port}
ca ca.crt
cert server.crt
key server.key
dh none
tls-crypt tc.key
auth SHA256
data-ciphers AES-256-GCM:AES-128-GCM
tls-version-min 1.3
keepalive 5 30
push \"redirect-gateway def1\"
push \"route 203.0.113.0 255.255.255.0\"
push \"dhcp-option DNS {SERVER}\"
verb 3
log server.log
",
            network = Ipv4Addr::from(u32::from(SERVER) & 0xffff_ff00),
        );
        std::fs::write(&path, profile).expect("server.ovpn");
        path
    }

    /// The client's: the directives of the work profile bliss was written
    /// for, with this run's keys inline.
    fn client(&self, port: u16) -> String {
        format!(
            "client
tls-client
auth SHA256
data-ciphers AES-256-GCM:AES-128-GCM
remote-cert-tls server
tls-version-min 1.3
proto udp
remote 127.0.0.1 {port}
dev tun
resolv-retry 5
nobind
keepalive 5 30
persist-key
persist-tun
verb 3
reneg-sec 0
route-method exe
route-delay 2
<ca>
{}</ca>
<cert>
{}</cert>
<key>
{}</key>
<tls-crypt>
{}</tls-crypt>
",
            self.read("ca.crt"),
            self.read("client.crt"),
            self.read("client.key"),
            self.read("tc.key"),
        )
    }
}

impl Drop for Kit {
    /// Tried for a while: an OpenVPN that was just ended holds its log open
    /// until it has gone, and the folder with it.
    fn drop(&mut self) {
        for _ in 0..40 {
            if std::fs::remove_dir_all(&self.dir).is_ok() || !self.dir.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!("{} is left behind", self.dir.display());
    }
}

/// The server's OpenVPN, ended when dropped.
struct Server(service::Started);

impl Drop for Server {
    fn drop(&mut self) {
        service::terminate(self.0.pid);
    }
}

/// A UDP port free right now.
fn free_port() -> u16 {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
    socket.local_addr().expect("its address").port()
}

/// Lines of PowerShell's answer.
fn powershell(script: &str) -> Vec<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .expect("powershell runs");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

/// The routes through one interface: prefix, next hop and metric.
fn routes(index: u32) -> Vec<String> {
    powershell(&format!(
        "Get-NetRoute -InterfaceIndex {index} -AddressFamily IPv4 -ErrorAction SilentlyContinue \
         | ForEach-Object {{ \"$($_.DestinationPrefix) $($_.NextHop) $($_.RouteMetric)\" }}"
    ))
}

/// Every route on the machine with bliss's metric.
fn routes_of_bliss() -> Vec<String> {
    powershell(
        "Get-NetRoute -AddressFamily IPv4 -ErrorAction SilentlyContinue \
         | Where-Object RouteMetric -eq 9999 \
         | ForEach-Object { \"$($_.DestinationPrefix) $($_.NextHop) $($_.InterfaceIndex)\" }",
    )
}

/// The DNS servers Windows was given for one interface.
fn dns_servers(index: u32) -> Vec<String> {
    powershell(&format!(
        "(Get-DnsClientServerAddress -InterfaceIndex {index} -AddressFamily IPv4 \
         -ErrorAction SilentlyContinue).ServerAddresses"
    ))
}

/// One of an interface's own byte counts: `SentBytes`, `ReceivedBytes`.
fn counted(what: &str, index: u32) -> u64 {
    powershell(&format!(
        "(Get-NetAdapter -InterfaceIndex {index} | Get-NetAdapterStatistics).{what}"
    ))
    .first()
    .and_then(|line| line.parse().ok())
    .unwrap_or_else(|| panic!("no {what} for interface {index}"))
}

/// Until the server says it is up, then the interface it opened.
///
/// By its log and the adapter it names, not by its address: a TAP adapter
/// keeps an address it was ever given, so an address alone can name the
/// adapter of a server from an earlier run.
async fn server_up(log: &Path, within: Duration) -> u32 {
    let deadline = Instant::now() + within;
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if text.contains("Initialization Sequence Completed") {
            let name = text
                .lines()
                .find_map(|line| {
                    line.split_once("tap-windows6 device [")?
                        .1
                        .split_once("] opened")
                })
                .map(|(name, _)| name.to_owned())
                .unwrap_or_else(|| panic!("the server names no adapter:\n{text}"));
            return powershell(&format!("(Get-NetAdapter -Name '{name}').ifIndex"))
                .first()
                .and_then(|line| line.parse().ok())
                .unwrap_or_else(|| panic!("no interface named {name}"));
        }
        assert!(
            Instant::now() < deadline,
            "the server did not come up; its log:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Until `address` is on no adapter, or `within` has passed.
async fn address_goes(address: Ipv4Addr, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if matches!(bliss_core::interface::index_of(address), Ok(None)) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Events until the VPN is up: its address and the DNS servers it reported.
async fn until_up(
    events: &mut mpsc::UnboundedReceiver<Event>,
    within: Duration,
) -> (Ipv4Addr, Vec<SocketAddr>) {
    let deadline = Instant::now() + within;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("the VPN came up in time")
            .expect("the session reports");
        println!("vpn: {event:?}");
        match event {
            Event::Up { local, dns } => return (local, dns),
            Event::Ended(why) => panic!("the VPN ended before it came up: {why:?}"),
            _ => {}
        }
    }
}

/// Events until the session ends: why, if it said.
async fn until_ended(
    events: &mut mpsc::UnboundedReceiver<Event>,
    within: Duration,
) -> Option<String> {
    let deadline = Instant::now() + within;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("the session ended in time")
            .expect("the session reports");
        println!("vpn: {event:?}");
        if let Event::Ended(why) = event {
            return why;
        }
    }
}

/// A session reporting into a channel.
fn session(profile: String, dir: &Path) -> (Session, mpsc::UnboundedReceiver<Event>) {
    let (tell, events) = mpsc::unbounded_channel();
    let session = Session::start(profile, dir.join("client"), None, move |event| {
        let _ = tell.send(event);
    });
    (session, events)
}

/// An echo of every TCP connection, on the server's side of the tunnel,
/// bound as soon as the server's address takes it: a new address is
/// tentative for a moment, and refuses to be bound until Windows has checked
/// nobody else on the link has it.
async fn echo() -> u16 {
    let deadline = Instant::now() + Duration::from_secs(10);
    let listener = loop {
        match TcpListener::bind((SERVER, 0)).await {
            Ok(listener) => break listener,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => panic!("the echo binds: {e}"),
        }
    };
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

/// A DNS server on the server's side that answers every A question with the
/// server's address, counting the questions.
async fn dns_server() -> Arc<AtomicUsize> {
    let socket = UdpSocket::bind((SERVER, 53))
        .await
        .expect("port 53 on the VPN's side");
    let asked = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    tokio::spawn(async move {
        let mut buf = [0u8; 1500];
        while let Ok((n, from)) = socket.recv_from(&mut buf).await {
            let query = &buf[..n];
            let Some(question_end) = question_end(query) else {
                continue;
            };
            count.fetch_add(1, Ordering::SeqCst);
            let mut reply = Vec::with_capacity(question_end + 16);
            reply.extend_from_slice(&query[..2]);
            reply.extend_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
            reply.extend_from_slice(&query[12..question_end]);
            reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
            reply.extend_from_slice(&SERVER.octets());
            let _ = socket.send_to(&reply, from).await;
        }
    });
    asked
}

/// Where the one question of a DNS query ends.
fn question_end(query: &[u8]) -> Option<usize> {
    let mut at = 12;
    loop {
        let length = usize::from(*query.get(at)?);
        if length == 0 {
            return (at + 5 <= query.len()).then_some(at + 5);
        }
        at += 1 + length;
    }
}

/// Bytes out and the same bytes back, through `way`.
async fn round_trip(way: &Through, target: Target, size: usize) {
    let mut stream = way
        .connect_tcp(&target)
        .await
        .unwrap_or_else(|e| panic!("dial {target:?} through the VPN: {e}"));
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let (mut read, mut write) = tokio::io::split(&mut stream);
    let sending = async {
        write.write_all(&payload).await.expect("sends");
        write.flush().await.expect("flushes");
    };
    let mut back = vec![0u8; size];
    let receiving = async { read.read_exact(&mut back).await.expect("reads it all back") };
    tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(sending, receiving)
    })
    .await
    .expect("the echo answered in time");
    assert!(back == payload, "the echo came back changed");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs OpenVPN Community and its service; starts two OpenVPNs"]
async fn a_work_vpn_carries_its_connections_and_nothing_else() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(std::env::var("BLISS_TEST_LOG").unwrap_or_else(|_| "info".to_owned()))
        .with_test_writer()
        .try_init();
    assert_eq!(
        bliss_openvpn::availability(),
        bliss_openvpn::Availability::Ready,
        "OpenVPN's interactive service is not running"
    );
    let kit = Kit::new();
    let port = free_port();

    // A profile OpenVPN will not take: said at once and in its own words,
    // not after the time bliss gives it to call.
    let started = Instant::now();
    let refused = format!("{}bliss-test-no-such-option\n", kit.client(port));
    let (_refused, mut events) = session(refused, &kit.dir);
    let why = until_ended(&mut events, Duration::from_secs(15)).await;
    println!("refused after {:?}: {why:?}", started.elapsed());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a refusal waited out the call"
    );
    assert!(
        why.as_deref()
            .is_some_and(|why| why.contains("Options error") && why.contains("no-such-option")),
        "{why:?}"
    );
    assert!(
        !kit.dir.join(r"client\profile.ovpn").exists(),
        "the refused profile is not left on disk"
    );

    // The server, through the same service, on the TAP adapter: the client
    // takes Data Channel Offload, as a work profile on Windows 11 does.
    let server_options = format!("--config {}", service::quoted(&kit.server(port)));
    let server = Server(service::start(&kit.dir, &server_options).expect("the server starts"));
    println!("server: process {}", server.0.pid);
    let server_index = server_up(&kit.dir.join("server.log"), Duration::from_secs(30)).await;
    println!("server: up on interface {server_index}");
    let echo_port = echo().await;
    let asked = dns_server().await;

    // The client, as bliss starts it.
    let started = Instant::now();
    let (client, mut events) = session(kit.client(port), &kit.dir);
    let (local, dns) = until_up(&mut events, Duration::from_secs(60)).await;
    println!("client: up as {local} after {:?}", started.elapsed());
    assert_eq!(
        dns,
        [SocketAddr::from((SERVER, 53))],
        "the pushed DNS server"
    );
    let index = bliss_core::interface::index_of(local)
        .expect("the interface table")
        .expect("the client's address is on an adapter");

    // Nothing the server pushed reached the machine; bliss's route did.
    let through_vpn = routes(index);
    println!("client: routes {through_vpn:?}");
    assert!(
        through_vpn
            .iter()
            .any(|route| route == &format!("0.0.0.0/0 {SERVER} 9999")),
        "bliss's own route through the VPN: {through_vpn:?}"
    );
    for pushed in ["0.0.0.0/1", "128.0.0.0/1", "203.0.113.0/24"] {
        assert!(
            !through_vpn
                .iter()
                .any(|route| route.starts_with(&format!("{pushed} "))),
            "{pushed} was pushed and must not be there: {through_vpn:?}"
        );
    }
    assert_eq!(
        dns_servers(index),
        Vec::<String>::new(),
        "DNS on the adapter"
    );

    // The profile, key and all, left the disk once OpenVPN had read it.
    assert!(
        !kit.dir.join(r"client\profile.ovpn").exists(),
        "the profile outlived the call"
    );

    // Through the client's adapter and back: a datagram, a connection by
    // address, and one by a name only the VPN's DNS server knows.
    let way = Through::new(
        index,
        local,
        dns,
        EngineInfo {
            kind: "openvpn",
            version: String::new(),
            external: true,
        },
    );
    let sink = UdpSocket::bind((SERVER, 9))
        .await
        .expect("a datagram sink on the VPN's side");
    let flow = way
        .bind_udp(&Target::Socket((SERVER, 9).into()))
        .await
        .expect("a flow through the VPN");
    let mut buf = [0u8; 64];
    let mut arrived = None;
    for _ in 0..5 {
        flow.send(b"ping").await.expect("sent");
        if let Ok(got) =
            tokio::time::timeout(Duration::from_secs(1), sink.recv_from(&mut buf)).await
        {
            arrived = Some(got.expect("received"));
            break;
        }
    }
    let (n, from) = arrived.expect("a datagram crossed the VPN");
    assert_eq!(&buf[..n], b"ping");
    assert_eq!(
        from.ip(),
        std::net::IpAddr::V4(local),
        "it came from the VPN's address"
    );
    sink.send_to(b"pong", from).await.expect("answered");
    let n = tokio::time::timeout(Duration::from_secs(5), flow.recv(&mut buf))
        .await
        .expect("the answer came back in time")
        .expect("received");
    assert_eq!(&buf[..n], b"pong");

    let (sent, received) = (counted("SentBytes", index), counted("ReceivedBytes", index));
    round_trip(&way, Target::Socket((SERVER, echo_port).into()), 256 * 1024).await;
    let (sent, received) = (
        counted("SentBytes", index) - sent,
        counted("ReceivedBytes", index) - received,
    );
    println!("client: 256 KB there and back, the adapter sent {sent} and received {received}");
    assert!(sent >= 256 * 1024, "the bytes left by the VPN's adapter");
    assert!(received >= 256 * 1024, "and came back by it");
    round_trip(&way, Target::domain(INTRANET, echo_port), 4096).await;
    assert!(
        asked.load(Ordering::SeqCst) >= 1,
        "the VPN's DNS server was asked"
    );

    // Asked to stop: gone, with its address and route.
    client.stop().await;
    let mut said_why = None;
    while let Ok(event) = events.try_recv() {
        println!("vpn: {event:?}");
        if let Event::Ended(why) = event {
            said_why = Some(why);
        }
    }
    assert_eq!(said_why, Some(None), "it ended because it was asked to");
    assert!(
        address_goes(local, Duration::from_secs(10)).await,
        "the client's address outlived it"
    );
    assert_eq!(
        routes_of_bliss(),
        Vec::<String>::new(),
        "bliss's route outlived it"
    );

    // The leash: a bliss that dies closes its side, and OpenVPN follows. The
    // session runs on a runtime of its own, which is then shut down under
    // it: its task is dropped mid-conversation and every socket it held
    // closes, as a dead process's do, with no `stop` and no `terminate`.
    let profile = kit.client(port);
    let dir = kit.dir.clone();
    let (up_tell, up) = tokio::sync::oneshot::channel();
    let (end_tell, end) = std::sync::mpsc::channel::<()>();
    let doomed = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let held = runtime.block_on(async {
            let (session, mut events) = session(profile, &dir);
            let (local, _) = until_up(&mut events, Duration::from_secs(60)).await;
            let _ = up_tell.send(local);
            (session, events)
        });
        let _ = end.recv();
        runtime.shutdown_background();
        drop(held);
    });
    let local = tokio::time::timeout(Duration::from_secs(70), up)
        .await
        .expect("up again in time")
        .expect("the second session came up");
    println!("leash: up as {local}; bliss dies");
    end_tell.send(()).expect("told to die");
    tokio::task::spawn_blocking(move || doomed.join())
        .await
        .expect("joined")
        .expect("the dying thread ran");
    assert!(
        address_goes(local, Duration::from_secs(15)).await,
        "OpenVPN outlived the bliss that started it"
    );
    assert_eq!(
        routes_of_bliss(),
        Vec::<String>::new(),
        "bliss's route outlived it"
    );
    drop(server);
}
