//! The engine's local ports against a program that is not bliss, with a real
//! xray (`BLISS_XRAY` names it), ignored by default:
//!
//! ```text
//! BLISS_XRAY=/path/to/xray cargo test -p bliss-xray --test datagrams -- --ignored --nocapture
//! ```
//!
//! The article of 7 April 2026 about VLESS clients says any app on a phone
//! can ask a client's local proxy where the tunnel comes out, and UDP gets
//! past a login. Here: the SOCKS port serves only its login and opens no UDP
//! socket, the datagrams go in over the Trojan inbound behind a password and
//! come back, and a datagram sent at either port from outside gets no answer
//! — while a flow's own datagrams are in the air.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use bliss_core::{Login, Outbound, SocksOutbound, Target};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

fn free_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn info() -> bliss_core::EngineInfo {
    bliss_core::EngineInfo {
        kind: "xray",
        version: String::new(),
        external: true,
    }
}

#[tokio::test]
#[ignore]
async fn the_ports_serve_only_bliss() {
    let exe = PathBuf::from(std::env::var_os("BLISS_XRAY").expect("BLISS_XRAY names an xray"));
    let dir = std::env::temp_dir().join(format!("bliss-datagrams-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // What the flow talks to: a UDP echo and a TCP one, on loopback.
    let echo_udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let udp_at = echo_udp.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = echo_udp.recv_from(&mut buf).await {
            let mut back = b"echo:".to_vec();
            back.extend_from_slice(&buf[..n]);
            let _ = echo_udp.send_to(&back, from).await;
        }
    });
    let echo_tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let tcp_at = echo_tcp.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = echo_tcp.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                if let Ok(n) = c.read(&mut buf).await {
                    let _ = c.write_all(&buf[..n]).await;
                }
            });
        }
    });

    // The config bliss writes, with the server swapped for the machine itself.
    let server: bliss_proto::ServerConfig =
        "trojan://PPPP@203.0.113.7:443?security=tls&sni=a.example#t"
            .parse()
            .unwrap();
    let (socks_port, door_port) = (free_port(), free_port());
    let login = Login {
        user: "uuuuuuuu".to_owned(),
        pass: "pppppppppppppppp".to_owned(),
    };
    let mut config = bliss_xray::build(
        &server,
        socks_port,
        bliss_xray::LogLevel::Warning,
        bliss_xray::Version::new(26, 3, 27),
    );
    bliss_xray::require_login(&mut config, &login.user, &login.pass);
    bliss_xray::datagrams_over_trojan(&mut config, door_port, &login.pass);
    config["outbounds"] = serde_json::json!([{ "protocol": "freedom" }]);
    let xray = bliss_xray::Xray::spawn(&exe, &config, &dir, socks_port)
        .await
        .expect("xray starts with both inbounds");
    let socks = xray.socks_addr();
    let door = SocketAddr::new(socks.ip(), door_port);
    // The Trojan inbound is up when xray is: it would not have started otherwise.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Neither port holds a UDP socket, so there is nothing for a stranger to send to.
    for port in [socks_port, door_port] {
        assert!(
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).is_ok(),
            "xray holds a UDP socket on {port}"
        );
    }

    // TCP through the login works and without it does not.
    let outbound = SocksOutbound::new(socks, info())
        .with_login(login.clone())
        .with_datagrams(door, login.pass.clone());
    let mut stream = outbound
        .connect_tcp(&Target::Socket(tcp_at))
        .await
        .expect("TCP with the login");
    stream.write_all(b"ping").await.unwrap();
    let mut got = [0u8; 4];
    stream.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"ping");
    assert!(
        SocksOutbound::new(socks, info())
            .connect_tcp(&Target::Socket(tcp_at))
            .await
            .is_err(),
        "the SOCKS port served a stranger"
    );

    // Datagrams go and come back, several, in order.
    let flow = outbound
        .bind_udp(&Target::Socket(udp_at))
        .await
        .expect("a flow of datagrams");
    for word in ["one", "two", "three"] {
        flow.send(word.as_bytes()).await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), flow.recv(&mut buf))
            .await
            .expect("an answer")
            .unwrap();
        assert_eq!(&buf[..n], format!("echo:{word}").as_bytes());
    }

    // With that flow still open, a stranger's datagram at either port — in
    // SOCKS's form, which is what the old association would have taken — gets
    // nothing back.
    let stranger = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let mut packet = vec![0, 0, 0, 1, 127, 0, 0, 1];
    packet.extend_from_slice(&udp_at.port().to_be_bytes());
    packet.extend_from_slice(b"who");
    for port in [socks_port, door_port] {
        stranger
            .send_to(&packet, (Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stranger.recv_from(&mut buf))
                .await
                .is_err(),
            "a stranger's datagram was answered at {port}"
        );
    }

    // A wrong password at the door gets nothing either.
    let wrong = SocksOutbound::new(socks, info())
        .with_login(login.clone())
        .with_datagrams(door, "not-the-password".to_owned());
    match wrong.bind_udp(&Target::Socket(udp_at)).await {
        Err(_) => {}
        Ok(flow) => {
            let _ = flow.send(b"who").await;
            let mut buf = [0u8; 64];
            let answered = tokio::time::timeout(Duration::from_secs(2), flow.recv(&mut buf)).await;
            assert!(
                !matches!(answered, Ok(Ok(n)) if n > 0),
                "the door took a wrong password"
            );
        }
    }

    let _ = xray.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
