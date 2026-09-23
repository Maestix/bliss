//! Every protocol and transport bliss writes, carried end to end by a real
//! xray-core on loopback: a server xray configured by hand, from Xray's own
//! config code; a client xray configured by `bliss_xray::build` from a share
//! link; and a TCP and a UDP echo behind them, reached through the client's
//! SOCKS port the way bliss reaches it.
//!
//! This is the check that a generated config not only parses but carries
//! bytes — a config that dials the wrong way passes `xray run -test`.
//! Ignored by default, since it needs xray.exe and starts two per case:
//!
//! ```text
//! cargo test -p bliss-xray --test engine -- --ignored
//! ```
//!
//! `BLISS_XRAY` names the executable; otherwise it is the one bliss installs,
//! `%LOCALAPPDATA%\bliss\data\xray.exe`. `BLISS_ENGINE_CASE` runs only the
//! cases whose names contain it, and `BLISS_ENGINE_DEBUG=1` has both xrays
//! log at debug level. Nothing leaves 127.0.0.1 — which on Windows
//! also keeps it out of any TUN adapter another client has up — and every
//! key, password and certificate is made for the run and thrown away.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bliss_proto::ServerConfig;
use bliss_xray::{LogLevel, Version, build, check_version};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

const ID: &str = "11111111-2222-3333-4444-555555555555";
const PASSWORD: &str = "PPPPPPPPPPPPPPPPPPPP";
const SNI: &str = "a.example";
const SHORT_ID: &str = "0123456789abcd";

fn exe() -> PathBuf {
    std::env::var_os("BLISS_XRAY").map_or_else(
        || {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set"))
                .join(r"bliss\data\xray.exe")
        },
        PathBuf::from,
    )
}

fn debug() -> bool {
    std::env::var_os("BLISS_ENGINE_DEBUG").is_some()
}

/// A port free for both TCP and UDP right now.
fn free_port() -> u16 {
    loop {
        let tcp = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = tcp.local_addr().unwrap().port();
        if std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return port;
        }
    }
}

/// Keys and a certificate for one run, made by xray itself.
struct Kit {
    dir: PathBuf,
    cert: PathBuf,
    key: PathBuf,
    /// The certificate's SHA-256, for `pcs`.
    pin: String,
    reality_private: String,
    reality_public: String,
    wg_server: (String, String),
    wg_client: (String, String),
    /// VLESS Encryption: the server's `decryption`, the client's `encryption`.
    vless_enc: (String, String),
}

fn run(args: &[&str]) -> String {
    let out = Command::new(exe())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("xray {args:?}: {e}"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn after(text: &str, label: &str) -> String {
    text.lines()
        .find_map(|line| line.trim().strip_prefix(label))
        .unwrap_or_else(|| panic!("no {label:?} in:\n{text}"))
        .trim()
        .trim_matches('"')
        .to_owned()
}

impl Kit {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("bliss-engine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("cert");
        run(&[
            "tls",
            "cert",
            &format!("-domain={SNI}"),
            &format!("-file={}", base.display()),
        ]);
        let cert = dir.join("cert.crt");
        let key = dir.join("cert.key");
        let pin = after(
            &run(&["tls", "hash", "--cert", &cert.display().to_string()]),
            "Leaf SHA256:",
        );

        // Fixed, obviously fake private keys; xray derives the public halves.
        let reality_private = URL_SAFE_NO_PAD.encode([0x11; 32]);
        let reality_public = after(
            &run(&["x25519", "-i", &reality_private]),
            "Password (PublicKey):",
        );
        let pair = |byte: u8| {
            let private = STANDARD.encode([byte; 32]);
            let public = after(&run(&["wg", "-i", &private]), "Password (PublicKey):");
            (private, public)
        };
        let enc = run(&["vlessenc"]);
        Self {
            cert,
            key,
            pin,
            reality_private,
            reality_public,
            wg_server: pair(0x22),
            wg_client: pair(0x33),
            vless_enc: (
                after(&enc, "\"decryption\":"),
                after(&enc, "\"encryption\":"),
            ),
            dir,
        }
    }

    fn tls(&self) -> Value {
        json!({
            "security": "tls",
            "tlsSettings": {
                "certificates": [{
                    "certificateFile": self.cert.display().to_string(),
                    "keyFile": self.key.display().to_string()
                }]
            }
        })
    }

    /// TLS for a Hysteria server, which must offer `h3`: without it the
    /// server refuses every client with TLS alert 120 (`CRYPTO_ERROR 0x178 …
    /// no application protocol` in the client's log), and the first run of
    /// this test failed that way. Real Hysteria servers always offer it.
    fn tls_h3(&self) -> Value {
        let mut tls = self.tls();
        tls["tlsSettings"]["alpn"] = json!(["h3"]);
        tls
    }

    fn reality(&self, target: u16) -> Value {
        json!({
            "security": "reality",
            "realitySettings": {
                "target": format!("127.0.0.1:{target}"),
                "serverNames": [SNI],
                "privateKey": self.reality_private,
                "shortIds": [SHORT_ID]
            }
        })
    }
}

impl Drop for Kit {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One server, as a link for bliss and an inbound for the server xray.
struct Case {
    name: &'static str,
    link: String,
    inbound: Value,
    /// Extra inbounds the server needs — REALITY's TLS target.
    beside: Vec<Value>,
}

/// Merge objects: `stream(&[a, b])` is `a` with `b`'s keys added.
fn merged(parts: &[Value]) -> Value {
    let mut out = json!({});
    for part in parts {
        for (k, v) in part.as_object().unwrap() {
            out[k] = v.clone();
        }
    }
    out
}

fn vless_in(flow: Option<&str>, decryption: &str, stream: Value) -> Value {
    let mut client = json!({ "id": ID });
    if let Some(flow) = flow {
        client["flow"] = json!(flow);
    }
    json!({
        "protocol": "vless",
        "settings": { "clients": [client], "decryption": decryption },
        "streamSettings": stream
    })
}

fn cases(kit: &Kit, port: u16, target: u16) -> Vec<Case> {
    let pcs = &kit.pin;
    let pbk = &kit.reality_public;
    let tls = || kit.tls();
    let reality = || kit.reality(target);
    let reality_target = || {
        vec![json!({
            "listen": "127.0.0.1",
            "port": target,
            "protocol": "vless",
            "settings": { "clients": [{ "id": ID }], "decryption": "none" },
            "streamSettings": merged(&[json!({ "network": "raw" }), kit.tls()])
        })]
    };
    let raw = || json!({ "network": "raw" });
    let at = format!("127.0.0.1:{port}");
    let tls_query = format!("security=tls&sni={SNI}&pcs={pcs}");
    let reality_query = format!("security=reality&sni={SNI}&pbk={pbk}&sid={SHORT_ID}&fp=chrome");
    let ss_user =
        |method: &str, password: &str| URL_SAFE_NO_PAD.encode(format!("{method}:{password}"));
    let ss_2022_key = STANDARD.encode([0x44; 16]);

    vec![
        Case {
            name: "vless raw",
            link: format!("vless://{ID}@{at}?encryption=none&type=tcp#c"),
            inbound: vless_in(None, "none", raw()),
            beside: vec![],
        },
        Case {
            name: "vless raw reality vision",
            link: format!("vless://{ID}@{at}?flow=xtls-rprx-vision&type=tcp&{reality_query}#c"),
            inbound: vless_in(
                Some("xtls-rprx-vision"),
                "none",
                merged(&[raw(), reality()]),
            ),
            beside: reality_target(),
        },
        Case {
            name: "vless raw reality vision, VLESS Encryption",
            link: format!(
                "vless://{ID}@{at}?encryption={}&flow=xtls-rprx-vision&type=tcp&{reality_query}#c",
                kit.vless_enc.1
            ),
            inbound: vless_in(
                Some("xtls-rprx-vision"),
                &kit.vless_enc.0,
                merged(&[raw(), reality()]),
            ),
            beside: reality_target(),
        },
        Case {
            name: "vless xhttp reality",
            link: format!("vless://{ID}@{at}?type=xhttp&path=%2Fx&mode=auto&{reality_query}#c"),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "xhttp", "xhttpSettings": { "path": "/x" } }),
                    reality(),
                ]),
            ),
            beside: reality_target(),
        },
        Case {
            name: "vless grpc reality",
            link: format!("vless://{ID}@{at}?type=grpc&serviceName=svc&{reality_query}#c"),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "grpc", "grpcSettings": { "serviceName": "svc" } }),
                    reality(),
                ]),
            ),
            beside: reality_target(),
        },
        Case {
            name: "vless ws tls",
            link: format!("vless://{ID}@{at}?type=ws&path=%2Fws&host={SNI}&{tls_query}#c"),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "ws", "wsSettings": { "path": "/ws" } }),
                    tls(),
                ]),
            ),
            beside: vec![],
        },
        // The link's own finalmask replaces bliss's; this one splits the TLS
        // ClientHello into pieces, which the server needs no setting to read.
        Case {
            name: "vless ws tls, the link's own finalmask",
            link: format!(
                "vless://{ID}@{at}?type=ws&path=%2Fws&{tls_query}\
                 &fm=%7B%22tcp%22%3A%5B%7B%22type%22%3A%22fragment%22%2C%22settings%22%3A%7B\
                 %22packets%22%3A%22tlshello%22%2C%22length%22%3A%2250-100%22%2C%22delay%22%3A\
                 %221-5%22%7D%7D%5D%7D#c"
            ),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "ws", "wsSettings": { "path": "/ws" } }),
                    tls(),
                ]),
            ),
            beside: vec![],
        },
        Case {
            name: "vless httpupgrade",
            link: format!("vless://{ID}@{at}?type=httpupgrade&path=%2Fup#c"),
            inbound: vless_in(
                None,
                "none",
                json!({ "network": "httpupgrade", "httpupgradeSettings": { "path": "/up" } }),
            ),
            beside: vec![],
        },
        Case {
            name: "vless xhttp tls, with extra",
            link: format!(
                "vless://{ID}@{at}?type=xhttp&path=%2Fx&mode=packet-up\
                 &extra=%7B%22xPaddingBytes%22%3A%22200-400%22%7D&{tls_query}&alpn=h2#c"
            ),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "xhttp", "xhttpSettings": { "path": "/x" } }),
                    tls(),
                ]),
            ),
            beside: vec![],
        },
        Case {
            name: "vless grpc multi tls",
            link: format!("vless://{ID}@{at}?type=grpc&serviceName=svc&mode=multi&{tls_query}#c"),
            inbound: vless_in(
                None,
                "none",
                merged(&[
                    json!({ "network": "grpc", "grpcSettings": { "serviceName": "svc" } }),
                    tls(),
                ]),
            ),
            beside: vec![],
        },
        Case {
            name: "vless raw with an http disguise",
            link: format!("vless://{ID}@{at}?type=tcp&headerType=http&host={SNI}&path=%2F#c"),
            inbound: vless_in(
                None,
                "none",
                json!({ "network": "raw", "rawSettings": { "header": { "type": "http" } } }),
            ),
            beside: vec![],
        },
        Case {
            name: "vless mkcp",
            link: format!("vless://{ID}@{at}?type=kcp#c"),
            inbound: vless_in(
                None,
                "none",
                json!({
                    "network": "kcp",
                    "finalmask": { "udp": [{ "type": "mkcp-original" }] }
                }),
            ),
            beside: vec![],
        },
        Case {
            name: "vless mkcp, seed and wechat disguise",
            link: format!("vless://{ID}@{at}?type=kcp&headerType=wechat-video&seed=SSSSSSSS#c"),
            inbound: vless_in(
                None,
                "none",
                json!({
                    "network": "kcp",
                    "finalmask": { "udp": [
                        { "type": "mkcp-aes128gcm", "settings": { "password": "SSSSSSSS" } },
                        { "type": "header-wechat" }
                    ] }
                }),
            ),
            beside: vec![],
        },
        Case {
            name: "vmess json ws tls",
            link: format!(
                "vmess://{}",
                STANDARD.encode(
                    json!({
                        "v": "2", "ps": "c", "add": "127.0.0.1", "port": port.to_string(),
                        "id": ID, "aid": "0", "scy": "auto", "net": "ws", "type": "none",
                        "host": SNI, "path": "/vm", "tls": "tls", "sni": SNI, "pcs": pcs
                    })
                    .to_string()
                )
            ),
            inbound: json!({
                "protocol": "vmess",
                "settings": { "clients": [{ "id": ID }] },
                "streamSettings": merged(&[
                    json!({ "network": "ws", "wsSettings": { "path": "/vm" } }),
                    tls(),
                ])
            }),
            beside: vec![],
        },
        Case {
            name: "vmess url raw",
            link: format!("vmess://{ID}@{at}?encryption=aes-128-gcm&type=tcp#c"),
            inbound: json!({
                "protocol": "vmess",
                "settings": { "clients": [{ "id": ID }] },
                "streamSettings": raw()
            }),
            beside: vec![],
        },
        Case {
            name: "trojan tls",
            link: format!("trojan://{PASSWORD}@{at}?sni={SNI}&pcs={pcs}#c"),
            inbound: json!({
                "protocol": "trojan",
                "settings": { "clients": [{ "password": PASSWORD }] },
                "streamSettings": merged(&[raw(), tls()])
            }),
            beside: vec![],
        },
        Case {
            name: "trojan raw reality",
            link: format!("trojan://{PASSWORD}@{at}?type=tcp&{reality_query}#c"),
            inbound: json!({
                "protocol": "trojan",
                "settings": { "clients": [{ "password": PASSWORD }] },
                "streamSettings": merged(&[raw(), reality()])
            }),
            beside: reality_target(),
        },
        Case {
            name: "shadowsocks aes-256-gcm",
            link: format!("ss://{}@{at}#c", ss_user("aes-256-gcm", PASSWORD)),
            inbound: json!({
                "protocol": "shadowsocks",
                "settings": { "method": "aes-256-gcm", "password": PASSWORD, "network": "tcp,udp" }
            }),
            beside: vec![],
        },
        Case {
            name: "shadowsocks 2022",
            link: format!(
                "ss://2022-blake3-aes-128-gcm:{}@{at}#c",
                ss_2022_key.replace('=', "%3D")
            ),
            inbound: json!({
                "protocol": "shadowsocks",
                "settings": {
                    "method": "2022-blake3-aes-128-gcm",
                    "password": ss_2022_key,
                    "network": "tcp,udp"
                }
            }),
            beside: vec![],
        },
        Case {
            name: "shadowsocks, the old link shape",
            link: format!(
                "ss://{}#c",
                STANDARD.encode(format!("chacha20-ietf-poly1305:{PASSWORD}@{at}"))
            ),
            inbound: json!({
                "protocol": "shadowsocks",
                "settings": {
                    "method": "chacha20-ietf-poly1305",
                    "password": PASSWORD,
                    "network": "tcp,udp"
                }
            }),
            beside: vec![],
        },
        // Without `tls`: a plugin's options have no room for a pinned
        // certificate, and this one is self-signed. Its TLS is read by the
        // same code as every other link's, which the other cases carry.
        Case {
            name: "shadowsocks as v2ray-plugin, websocket",
            link: format!(
                "ss://{}@{at}/?plugin=v2ray-plugin%3Bhost%3D{SNI}%3Bpath%3D%2Fss#c",
                ss_user("aes-128-gcm", PASSWORD)
            ),
            inbound: json!({
                "protocol": "shadowsocks",
                "settings": { "method": "aes-128-gcm", "password": PASSWORD, "network": "tcp,udp" },
                "streamSettings": { "network": "ws", "wsSettings": { "path": "/ss" } }
            }),
            beside: vec![],
        },
        Case {
            name: "shadowsocks as obfs-local, an http disguise",
            link: format!(
                "ss://{}@{at}/?plugin=obfs-local%3Bobfs%3Dhttp%3Bobfs-host%3D{SNI}#c",
                ss_user("aes-128-gcm", PASSWORD)
            ),
            inbound: json!({
                "protocol": "shadowsocks",
                "settings": { "method": "aes-128-gcm", "password": PASSWORD, "network": "tcp,udp" },
                "streamSettings": { "network": "raw", "rawSettings": { "header": { "type": "http" } } }
            }),
            beside: vec![],
        },
        Case {
            name: "socks with a login",
            link: format!(
                "socks://{}@{at}#c",
                URL_SAFE_NO_PAD.encode(format!("user:{PASSWORD}"))
            ),
            inbound: json!({
                "protocol": "socks",
                "settings": {
                    "auth": "password",
                    "accounts": [{ "user": "user", "pass": PASSWORD }],
                    "udp": true,
                    "ip": "127.0.0.1"
                }
            }),
            beside: vec![],
        },
        Case {
            name: "hysteria2",
            link: format!("hysteria2://{PASSWORD}@{at}/?sni={SNI}&pinSHA256={pcs}#c"),
            inbound: json!({
                "protocol": "hysteria",
                "settings": { "version": 2, "clients": [{ "auth": PASSWORD }] },
                "streamSettings": merged(&[
                    json!({ "network": "hysteria", "hysteriaSettings": { "version": 2 } }),
                    kit.tls_h3(),
                ])
            }),
            beside: vec![],
        },
        // Hopping between ports that are all the server's one port: what is
        // checked is that xray takes bliss's udpHop and still connects.
        Case {
            name: "hysteria2 hopping ports",
            link: format!("hy2://{PASSWORD}@{at}/?sni={SNI}&pinSHA256={pcs}&mport={port}#c"),
            inbound: json!({
                "protocol": "hysteria",
                "settings": { "version": 2, "clients": [{ "auth": PASSWORD }] },
                "streamSettings": merged(&[
                    json!({ "network": "hysteria", "hysteriaSettings": { "version": 2 } }),
                    kit.tls_h3(),
                ])
            }),
            beside: vec![],
        },
        Case {
            name: "hysteria2 with salamander",
            link: format!(
                "hy2://{PASSWORD}@{at}/?sni={SNI}&pinSHA256={pcs}&obfs=salamander&obfs-password=OOOOOOOO#c"
            ),
            inbound: json!({
                "protocol": "hysteria",
                "settings": { "version": 2, "clients": [{ "auth": PASSWORD }] },
                "streamSettings": merged(&[
                    json!({
                        "network": "hysteria",
                        "hysteriaSettings": { "version": 2 },
                        "finalmask": { "udp": [
                            { "type": "salamander", "settings": { "password": "OOOOOOOO" } }
                        ] }
                    }),
                    kit.tls_h3(),
                ])
            }),
            beside: vec![],
        },
        Case {
            name: "wireguard",
            link: format!(
                "wireguard://{}@{at}?publickey={}&address=10.7.0.2%2F32&mtu=1420#c",
                kit.wg_client
                    .0
                    .replace('=', "%3D")
                    .replace('+', "%2B")
                    .replace('/', "%2F"),
                kit.wg_server
                    .1
                    .replace('=', "%3D")
                    .replace('+', "%2B")
                    .replace('/', "%2F")
            ),
            inbound: json!({
                "protocol": "wireguard",
                "settings": {
                    "secretKey": kit.wg_server.0,
                    "peers": [{ "publicKey": kit.wg_client.1, "allowedIPs": ["10.7.0.2/32"] }]
                }
            }),
            beside: vec![],
        },
    ]
}

/// A child xray, killed when dropped, whose output lands in a file.
struct Running {
    child: Child,
    log: PathBuf,
}

impl Running {
    fn start(config: &Value, dir: &Path, name: &str) -> Self {
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(config).unwrap()).unwrap();
        let log = dir.join(format!("{name}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(exe())
            .args(["run", "-c"])
            .arg(&path)
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .stdin(Stdio::null())
            .spawn()
            .expect("xray starts");
        Self { child, log }
    }

    fn output(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn echo_tcp(listener: TcpListener) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stream.read(&mut buf).await {
                if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });
    }
}

async fn echo_udp(socket: UdpSocket) {
    let mut buf = [0u8; 4096];
    while let Ok((n, from)) = socket.recv_from(&mut buf).await {
        let _ = socket.send_to(&buf[..n], from).await;
    }
}

async fn socks_greet(socks: u16) -> std::io::Result<TcpStream> {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, socks)).await?;
    stream.write_all(&[5, 1, 0]).await?;
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await?;
    Ok(stream)
}

/// Bytes to the TCP echo through the client's SOCKS port, and back.
async fn through_tcp(
    socks: u16,
    dest: [u8; 4],
    target: u16,
    payload: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut stream = socks_greet(socks).await?;
    let port = target.to_be_bytes();
    stream
        .write_all(&[
            5, 1, 0, 1, dest[0], dest[1], dest[2], dest[3], port[0], port[1],
        ])
        .await?;
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).await?;
    if reply[1] != 0 {
        return Err(std::io::Error::other(format!("SOCKS reply {}", reply[1])));
    }
    stream.write_all(payload).await?;
    let mut back = vec![0u8; payload.len()];
    stream.read_exact(&mut back).await?;
    Ok(back)
}

/// A datagram to the UDP echo through the client's SOCKS port, and back.
async fn through_udp(
    socks: u16,
    dest: [u8; 4],
    target: u16,
    payload: &[u8],
) -> std::io::Result<Vec<u8>> {
    let mut control = socks_greet(socks).await?;
    control.write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    let mut reply = [0u8; 10];
    control.read_exact(&mut reply).await?;
    if reply[1] != 0 || reply[3] != 1 {
        return Err(std::io::Error::other(format!(
            "UDP ASSOCIATE reply {reply:?}"
        )));
    }
    let relay = SocketAddr::from(([127, 0, 0, 1], u16::from_be_bytes([reply[8], reply[9]])));
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = target.to_be_bytes();
    let mut datagram = vec![
        0, 0, 0, 1, dest[0], dest[1], dest[2], dest[3], port[0], port[1],
    ];
    datagram.extend_from_slice(payload);
    // The first datagram of a flow can race the flow's setup: ask again.
    for _ in 0..5 {
        socket.send_to(&datagram, relay).await?;
        let mut buf = [0u8; 4096];
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buf)).await
        {
            return Ok(buf[10.min(n)..n].to_vec());
        }
    }
    drop(control);
    Err(std::io::Error::other("no datagram came back"))
}

/// Wait until the client answers on its SOCKS port.
async fn ready(socks: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if socks_greet(socks).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "starts real xray processes; run with --ignored"]
async fn every_protocol_carries_tcp_and_udp_through_a_real_xray() {
    let version = check_version(&exe(), None)
        .await
        .expect("an xray to test against");
    let kit = Kit::new();

    // One port for both echoes, so a redirect can name it once.
    let echo = free_port();
    let tcp_echo = TcpListener::bind((Ipv4Addr::LOCALHOST, echo))
        .await
        .unwrap();
    tokio::spawn(echo_tcp(tcp_echo));
    let udp_echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, echo)).await.unwrap();
    tokio::spawn(echo_udp(udp_echo));

    let mut failures = Vec::new();
    let only = std::env::var("BLISS_ENGINE_CASE").unwrap_or_default();
    let (server_port, reality_target) = (free_port(), free_port());
    let count = cases(&kit, server_port, reality_target).len();
    for index in 0..count {
        // Fresh ports every case: a UDP port lingers after its process.
        let (server_port, reality_target, socks) = (free_port(), free_port(), free_port());
        let case = cases(&kit, server_port, reality_target).swap_remove(index);
        if !case.name.contains(&only) {
            continue;
        }
        let outcome = one_case(&case, &kit, version, server_port, socks, echo).await;
        match outcome {
            Ok(()) => println!("ok    {}", case.name),
            Err(why) => {
                println!(
                    "FAIL  {}: {}",
                    case.name,
                    why.lines().next().unwrap_or_default()
                );
                failures.push(format!("{}: {why}", case.name));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {count} failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

async fn one_case(
    case: &Case,
    kit: &Kit,
    version: Version,
    server_port: u16,
    socks: u16,
    echo: u16,
) -> Result<(), String> {
    // A WireGuard server hands the tunnel's packets to a gVisor netstack,
    // which drops any addressed to loopback as martian — the handshake then
    // succeeds and nothing else does. So that case asks for an address in
    // TEST-NET-3, and the server's freedom sends everything to the echo.
    let netstack = case.inbound["protocol"] == "wireguard";
    let (dest, outbound) = if netstack {
        (
            [203, 0, 113, 1],
            json!({ "protocol": "freedom", "settings": { "redirect": format!("127.0.0.1:{echo}") } }),
        )
    } else {
        ([127, 0, 0, 1], json!({ "protocol": "freedom" }))
    };
    let mut inbound = case.inbound.clone();
    inbound["listen"] = json!("127.0.0.1");
    inbound["port"] = json!(server_port);
    let mut inbounds = vec![inbound];
    inbounds.extend(case.beside.iter().cloned());
    let server_config = json!({
        "log": { "loglevel": if debug() { "debug" } else { "warning" } },
        "inbounds": inbounds,
        "outbounds": [outbound]
    });
    let slug: String = case
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let server = Running::start(&server_config, &kit.dir, &format!("{slug}-server"));

    let parsed: ServerConfig = case
        .link
        .parse()
        .map_err(|e| format!("the link does not parse: {e}"))?;
    let level = if debug() {
        LogLevel::Debug
    } else {
        LogLevel::Warning
    };
    let client_config = build(&parsed, socks, level, version);
    let client = Running::start(&client_config, &kit.dir, &format!("{slug}-client"));
    let logs = |what: &str| {
        format!(
            "{what}\n--- server ---\n{}\n--- client ---\n{}\n--- client config ---\n{}",
            server.output(),
            client.output(),
            serde_json::to_string_pretty(&client_config).unwrap()
        )
    };
    if !ready(socks).await {
        return Err(logs("the client never listened"));
    }
    // A server on UDP has nothing to connect to; give it a moment to bind.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let tcp_payload = format!("tcp through {}", case.name).into_bytes();
    match tokio::time::timeout(
        Duration::from_secs(10),
        through_tcp(socks, dest, echo, &tcp_payload),
    )
    .await
    {
        Ok(Ok(back)) if back == tcp_payload => {}
        Ok(Ok(back)) => return Err(logs(&format!("tcp came back as {back:?}"))),
        Ok(Err(e)) => return Err(logs(&format!("tcp: {e}"))),
        Err(_) => return Err(logs("tcp: timed out")),
    }

    let udp_payload = format!("udp through {}", case.name).into_bytes();
    match tokio::time::timeout(
        Duration::from_secs(12),
        through_udp(socks, dest, echo, &udp_payload),
    )
    .await
    {
        Ok(Ok(back)) if back == udp_payload => Ok(()),
        Ok(Ok(back)) => Err(logs(&format!("udp came back as {back:?}"))),
        Ok(Err(e)) => Err(logs(&format!("udp: {e}"))),
        Err(_) => Err(logs("udp: timed out")),
    }
}
