//! Link parsing, checked against the shapes that actually turn up in the wild.
//!
//! Every link here is synthetic: same structure as a real one, TEST-NET or
//! reserved addresses, key material and passwords that are obviously fake.
//! Real credentials belong in `bliss.local.toml`, which is gitignored.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bliss_proto::{
    Address, AppearanceMode, Encryption, EncryptionKey, Fingerprint, Flow, KcpHeader, LinkError,
    Protocol, RttMode, Security, ServerConfig, ShortId, TcpHeader, Transport, Unsupported,
    VmessCipher,
};

/// REALITY + Vision + the ML-KEM-768/X25519 encryption layer Xray 25.x added.
/// This is the shape of a current, fully-loaded config.
const REALITY_LINK: &str = "vless://11111111-2222-3333-4444-555555555555@203.0.113.10:443\
    ?encryption=mlkem768x25519plus.native.0rtt.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\
    &flow=xtls-rprx-vision\
    &security=reality\
    &sni=store.steampowered.com\
    &fp=firefox\
    &pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ\
    &sid=0123456789abcd\
    &spx=%2Fdeadbeef\
    &type=tcp\
    &headerType=none\
    #test-reserved";

/// Classic VLESS over plain TLS: no inner encryption, no flow.
const PLAIN_TLS_LINK: &str = "vless://11111111-2222-3333-4444-555555555555@example.net:8443\
    ?encryption=none&security=tls&sni=example.net&alpn=h2,http/1.1#plain";

/// The password every password-carrying link here uses: long, and easy to
/// search for in `Debug` output.
const PASSWORD: &str = "PPPPPPPPPPPPPPPPPPPP";

fn parse(link: &str) -> ServerConfig {
    link.parse()
        .unwrap_or_else(|e| panic!("{link} should parse: {e}"))
}

fn refusal(link: &str) -> LinkError {
    link.parse::<ServerConfig>()
        .expect_err("the link should be refused")
}

fn vless(cfg: &ServerConfig) -> (&Encryption, Flow) {
    match &cfg.protocol {
        Protocol::Vless {
            encryption, flow, ..
        } => (encryption, *flow),
        other => panic!("expected VLESS, got {other:?}"),
    }
}

#[test]
fn parses_reality_with_vless_encryption() {
    let cfg = parse(REALITY_LINK);

    assert_eq!(cfg.tag, "test-reserved");
    assert_eq!(cfg.address, Address::Ip("203.0.113.10".parse().unwrap()));
    assert_eq!(cfg.port, 443);
    assert_eq!(vless(&cfg).1, Flow::XtlsRprxVision);
    assert_eq!(
        cfg.transport,
        Transport::Tcp {
            header: TcpHeader::None
        }
    );

    let Security::Reality(r) = &cfg.security else {
        panic!("expected reality, got {:?}", cfg.security);
    };
    assert_eq!(r.sni, "store.steampowered.com");
    assert_eq!(r.fingerprint, Fingerprint::Firefox);
    assert_eq!(r.spider_x.as_deref(), Some("/deadbeef"));

    // Seven bytes of short id, zero-padded on the right to the eight that go
    // into the ClientHello session id.
    assert_eq!(r.short_id.to_string(), "0123456789abcd");
    assert_eq!(
        r.short_id.padded(),
        [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0x00]
    );

    let Encryption::MlKem768X25519Plus(e) = vless(&cfg).0 else {
        panic!("expected the encryption layer, got {:?}", vless(&cfg).0);
    };
    assert_eq!(e.mode, AppearanceMode::Native);
    assert_eq!(e.rtt, RttMode::Zero);
    assert!(matches!(e.keys.as_slice(), [EncryptionKey::X25519(_)]));
}

#[test]
fn parsed_reality_link_matches_snapshot() {
    insta::assert_debug_snapshot!(parse(REALITY_LINK));
}

/// The whole point of the newtypes in `server.rs`. A ServerConfig reaches the
/// log pane and error reports; if this test ever fails, live credentials are
/// being written somewhere a user will paste into a bug report.
#[test]
fn debug_does_not_leak_credentials() {
    let rendered = format!("{:?}", parse(REALITY_LINK));

    assert!(
        !rendered.contains("11111111-2222-3333-4444-555555555555"),
        "the full UUID leaked into Debug output:\n{rendered}"
    );
    assert!(
        !rendered.contains("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ"),
        "the full Reality public key leaked into Debug output:\n{rendered}"
    );
    // Redacted, not erased: enough to tell two servers apart at a glance.
    assert!(rendered.contains("11111111…5555"), "got:\n{rendered}");
}

/// The same for every protocol that carries a password or a private key,
/// where not even part of the value may show: half a password is most of it.
#[test]
fn no_protocol_leaks_its_secret_into_debug() {
    let secret_key = "A".repeat(43) + "=";
    let pre_shared = "C".repeat(42) + "A=";
    let links = [
        format!("trojan://{PASSWORD}@example.net:443?sni=example.net#t"),
        format!(
            "ss://{}@example.net:8388#s",
            URL_SAFE_NO_PAD.encode(format!("aes-256-gcm:{PASSWORD}"))
        ),
        format!("socks5://UUUUUUUUUUUU:{PASSWORD}@example.net:1080#k"),
        format!(
            "hysteria2://{PASSWORD}@example.net:443?obfs=salamander&obfs-password=OOOOOOOOOOOOOOOO#h"
        ),
        format!(
            "wireguard://{}@203.0.113.7:51820?publickey={}&presharedkey={}#w",
            secret_key.replace('=', "%3D"),
            "B".repeat(42) + "Q%3D",
            pre_shared.replace('=', "%3D")
        ),
        format!(
            "vless://11111111-2222-3333-4444-555555555555@example.net:443\
             ?type=xhttp&extra=%7B%22headers%22%3A%7B%22X-Key%22%3A%22{PASSWORD}%22%7D%7D\
             &fm=%7B%22udp%22%3A%5B%7B%22type%22%3A%22salamander%22%2C%22settings%22%3A%7B%22password%22%3A%22{PASSWORD}%22%7D%7D%5D%7D"
        ),
    ];
    for link in &links {
        let rendered = format!("{:?}", parse(link));
        for secret in [
            PASSWORD,
            "UUUUUUUUUUUU",
            "OOOOOOOOOOOOOOOO",
            &secret_key[..20],
            &pre_shared[..20],
            &STANDARD.encode([0u8; 32])[..20],
        ] {
            assert!(
                !rendered.contains(secret),
                "a secret leaked into Debug output of {link}:\n{rendered}"
            );
        }
    }
}

/// Stage 1 hands this string to xray-core verbatim, so it has to survive the
/// round trip byte for byte.
#[test]
fn encryption_parameter_round_trips() {
    assert_eq!(
        vless(&parse(REALITY_LINK)).0.as_param(),
        "mlkem768x25519plus.native.0rtt.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    );
    assert_eq!(vless(&parse(PLAIN_TLS_LINK)).0.as_param(), "none");
}

#[test]
fn parses_plain_tls() {
    let cfg = parse(PLAIN_TLS_LINK);

    assert_eq!(cfg.address, Address::Domain("example.net".to_owned()));
    assert_eq!(vless(&cfg), (&Encryption::None, Flow::None));

    let Security::Tls(t) = &cfg.security else {
        panic!("expected tls, got {:?}", cfg.security);
    };
    assert_eq!(t.alpn, ["h2", "http/1.1"]);
    assert!(!t.allow_insecure);
    // No fp in the link: we parrot Chrome unless told otherwise.
    assert_eq!(t.fingerprint, Fingerprint::Chrome);
}

#[test]
fn tls_takes_pinned_certificates_names_and_ech() {
    let pin = "ab".repeat(32);
    let cfg = parse(&format!(
        "vless://11111111-2222-3333-4444-555555555555@example.net:443\
         ?security=tls&allowInsecure=1&pcs={pin}&vcn=a.example,b.example&ech=AAAA"
    ));
    let Security::Tls(t) = &cfg.security else {
        panic!("expected tls, got {:?}", cfg.security);
    };
    assert!(t.allow_insecure);
    assert_eq!(t.pinned, [pin]);
    assert_eq!(t.verify_names, ["a.example", "b.example"]);
    assert_eq!(t.ech.as_deref(), Some("AAAA"));

    let err = refusal(
        "vless://11111111-2222-3333-4444-555555555555@example.net:443?security=tls&pcs=abcd",
    );
    assert!(
        matches!(err, LinkError::BadValue { param: "pcs", .. }),
        "got {err:?}"
    );
}

#[test]
fn tag_falls_back_to_address_when_the_fragment_is_missing() {
    let cfg = parse("vless://11111111-2222-3333-4444-555555555555@example.net:443?encryption=none");
    assert_eq!(cfg.tag, "example.net:443");
}

#[test]
fn reads_every_transport_xray_has() {
    const HEAD: &str = "vless://11111111-2222-3333-4444-555555555555@example.net:443?";
    let transport = |query: &str| parse(&format!("{HEAD}{query}")).transport;

    assert_eq!(
        transport("type=ws&path=%2Fsocket%3Fed%3D2048&host=cdn.example"),
        Transport::Ws {
            path: "/socket?ed=2048".to_owned(),
            host: Some("cdn.example".to_owned())
        }
    );
    assert_eq!(
        transport("type=httpupgrade"),
        Transport::HttpUpgrade {
            path: "/".to_owned(),
            host: None
        }
    );
    let Transport::Xhttp {
        path, mode, extra, ..
    } = transport("type=xhttp&path=%2Fx&mode=stream-one&extra=%7B%22noSSEHeader%22%3Atrue%7D")
    else {
        panic!("expected xhttp");
    };
    assert_eq!((path.as_str(), mode.as_str()), ("/x", "stream-one"));
    assert_eq!(
        extra.map(|e| e.0).as_deref(),
        Some(r#"{"noSSEHeader":true}"#)
    );
    // `splithttp` is its old name, and defaults to `auto`.
    assert!(matches!(
        transport("type=splithttp"),
        Transport::Xhttp { mode, .. } if mode == "auto"
    ));
    assert_eq!(
        transport("type=grpc&serviceName=svc&authority=a.example&mode=multi"),
        Transport::Grpc {
            service_name: "svc".to_owned(),
            authority: Some("a.example".to_owned()),
            multi: true
        }
    );
    let Transport::Kcp { header, seed, mtu } =
        transport("type=kcp&headerType=wechat-video&seed=SSSSSSSS&mtu=1350")
    else {
        panic!("expected kcp");
    };
    assert_eq!(header, KcpHeader::Wechat);
    assert_eq!(seed.map(|s| s.0).as_deref(), Some("SSSSSSSS"));
    assert_eq!(mtu, Some(1350));
    assert_eq!(
        transport("type=tcp&headerType=http&host=a.example,b.example"),
        Transport::Tcp {
            header: TcpHeader::Http {
                host: vec!["a.example".to_owned(), "b.example".to_owned()],
                path: vec!["/".to_owned()]
            }
        }
    );
}

/// HTTP/2 and QUIC were removed from Xray; a link that asks for them can only
/// be refused, and should say which transport it was.
#[test]
fn rejects_transports_xray_no_longer_has() {
    for (kind, name) in [("h2", "h2"), ("http", "http"), ("quic", "quic")] {
        let err = refusal(&format!(
            "vless://11111111-2222-3333-4444-555555555555@example.net:443?type={kind}"
        ));
        assert!(
            matches!(&err, LinkError::UnsupportedTransport(t) if t == name),
            "got {err:?}"
        );
    }
}

/// xray refuses REALITY over anything else, and says so only when it starts.
#[test]
fn reality_goes_over_tcp_xhttp_and_grpc_only() {
    const HEAD: &str = "vless://11111111-2222-3333-4444-555555555555@example.net:443\
                        ?security=reality&sni=a.example&pbk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBQ";
    for fine in ["tcp", "xhttp", "grpc"] {
        parse(&format!("{HEAD}&type={fine}"));
    }
    let err = refusal(&format!("{HEAD}&type=ws"));
    assert!(
        matches!(err, LinkError::BadValue { param: "type", .. }),
        "got {err:?}"
    );
}

#[test]
fn rejects_a_public_key_of_the_wrong_length() {
    let err = refusal(
        "vless://11111111-2222-3333-4444-555555555555@example.net:443\
        ?security=reality&sni=a.example&pbk=AAAA&sid=ab",
    );
    assert!(
        matches!(&err, LinkError::BadValue { param: "pbk", .. }),
        "got {err:?}"
    );
}

#[test]
fn rejects_reality_without_a_public_key() {
    let err = refusal(
        "vless://11111111-2222-3333-4444-555555555555@example.net:443\
        ?security=reality&sni=a.example",
    );
    assert!(
        matches!(err, LinkError::RealityMissing("pbk")),
        "got {err:?}"
    );
}

#[test]
fn vmess_reads_v2rayns_json_and_the_url_form() {
    let json = serde_json::json!({
        "v": "2", "ps": "vm", "add": "203.0.113.5", "port": 443,
        "id": "11111111-2222-3333-4444-555555555555", "aid": "0", "scy": "aes-128-gcm",
        "net": "ws", "type": "none", "host": "cdn.example", "path": "/vm",
        "tls": "tls", "sni": "cdn.example", "fp": "safari"
    });
    let cfg = parse(&format!("vmess://{}", STANDARD.encode(json.to_string())));
    assert_eq!(cfg.tag, "vm");
    assert_eq!(cfg.port, 443);
    assert!(matches!(
        cfg.protocol,
        Protocol::Vmess {
            cipher: VmessCipher::Aes128Gcm,
            ..
        }
    ));
    assert_eq!(
        cfg.transport,
        Transport::Ws {
            path: "/vm".to_owned(),
            host: Some("cdn.example".to_owned())
        }
    );
    let Security::Tls(t) = &cfg.security else {
        panic!("expected tls, got {:?}", cfg.security);
    };
    assert_eq!(
        (t.sni.as_deref(), &t.fingerprint),
        (Some("cdn.example"), &Fingerprint::Safari)
    );

    // gRPC keeps its service name where a path would be; the port may be text.
    let json = serde_json::json!({
        "add": "example.net", "port": "8443", "id": "11111111-2222-3333-4444-555555555555",
        "net": "grpc", "type": "multi", "path": "svc"
    });
    let cfg = parse(&format!(
        "vmess://{}",
        URL_SAFE_NO_PAD.encode(json.to_string())
    ));
    assert_eq!(cfg.tag, "example.net:8443");
    assert_eq!(
        cfg.transport,
        Transport::Grpc {
            service_name: "svc".to_owned(),
            authority: None,
            multi: true
        }
    );

    let cfg = parse(
        "vmess://11111111-2222-3333-4444-555555555555@example.net:443\
         ?encryption=chacha20-poly1305&type=httpupgrade&security=tls#u",
    );
    assert!(matches!(
        cfg.protocol,
        Protocol::Vmess {
            cipher: VmessCipher::Chacha20Poly1305,
            ..
        }
    ));

    // Neither base64 nor JSON: described, never quoted.
    let err = refusal("vmess://bm90IGpzb24=");
    assert!(
        matches!(&err, LinkError::BadValue { param: "vmess", got, .. } if !got.contains("bm90")),
        "got {err:?}"
    );
}

#[test]
fn trojan_is_tls_unless_it_says_otherwise() {
    let cfg = parse(&format!(
        "trojan://{PASSWORD}@example.net:443?sni=a.example&type=grpc&serviceName=t#tr"
    ));
    assert!(matches!(&cfg.protocol, Protocol::Trojan { password } if password.0 == PASSWORD));
    assert!(matches!(&cfg.security, Security::Tls(t) if t.sni.as_deref() == Some("a.example")));
    assert!(matches!(cfg.transport, Transport::Grpc { .. }));

    // A colon in the password is part of it, not the start of another field.
    let cfg = parse("trojan://pa%3Ass:word@example.net:443?security=none");
    assert!(matches!(&cfg.protocol, Protocol::Trojan { password } if password.0 == "pa:ss:word"));
    assert!(matches!(cfg.security, Security::None));

    assert!(matches!(
        refusal("trojan://example.net:443"),
        LinkError::Missing("password")
    ));
}

#[test]
fn shadowsocks_reads_every_shape() {
    let method = |cfg: &ServerConfig| match &cfg.protocol {
        Protocol::Shadowsocks { method, password } => (method.clone(), password.0.clone()),
        other => panic!("expected Shadowsocks, got {other:?}"),
    };

    // SIP002 with base64 user info.
    let cfg = parse(&format!(
        "ss://{}@203.0.113.9:8388#sip",
        URL_SAFE_NO_PAD.encode(format!("chacha20-ietf-poly1305:{PASSWORD}"))
    ));
    assert_eq!(
        method(&cfg),
        ("chacha20-ietf-poly1305".to_owned(), PASSWORD.to_owned())
    );
    assert_eq!(cfg.tag, "sip");

    // 2022 ciphers are written out, the key percent-encoded; a multi-user
    // server's two keys keep the colon between them.
    let cfg = parse(
        "ss://2022-blake3-aes-128-gcm:AAAAAAAAAAAAAAAAAAAAAA%3D%3D%3ABBBBBBBBBBBBBBBBBBBBBA%3D%3D@example.net:443",
    );
    assert_eq!(
        method(&cfg),
        (
            "2022-blake3-aes-128-gcm".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAA==:BBBBBBBBBBBBBBBBBBBBBA==".to_owned()
        )
    );

    // The old shape: everything in base64.
    let cfg = parse(&format!(
        "ss://{}#old",
        STANDARD.encode(format!("aes-128-gcm:{PASSWORD}@203.0.113.9:8388"))
    ));
    assert_eq!(method(&cfg).0, "aes-128-gcm");
    assert_eq!(
        (cfg.address.to_string(), cfg.port),
        ("203.0.113.9".to_owned(), 8388)
    );

    let userinfo = URL_SAFE_NO_PAD.encode(format!("aes-256-gcm:{PASSWORD}"));
    // obfs-local's HTTP disguise is a TCP header in xray.
    let cfg = parse(&format!(
        "ss://{userinfo}@example.net:80/?plugin=obfs-local%3Bobfs%3Dhttp%3Bobfs-host%3Da.example"
    ));
    assert_eq!(
        cfg.transport,
        Transport::Tcp {
            header: TcpHeader::Http {
                host: vec!["a.example".to_owned()],
                path: vec!["/".to_owned()]
            }
        }
    );
    // v2ray-plugin is WebSocket, here with TLS.
    let cfg = parse(&format!(
        "ss://{userinfo}@example.net:443/?plugin=v2ray-plugin%3Btls%3Bhost%3Da.example%3Bpath%3D%2Fws"
    ));
    assert!(matches!(&cfg.security, Security::Tls(t) if t.sni.as_deref() == Some("a.example")));
    assert_eq!(
        cfg.transport,
        Transport::Ws {
            path: "/ws".to_owned(),
            host: Some("a.example".to_owned())
        }
    );

    let stream_cipher = URL_SAFE_NO_PAD.encode(format!("aes-256-cfb:{PASSWORD}"));
    assert!(matches!(
        refusal(&format!("ss://{stream_cipher}@example.net:8388")),
        LinkError::Unsupported(Unsupported::Cipher(c)) if c == "aes-256-cfb"
    ));
    assert!(matches!(
        refusal(&format!("ss://{userinfo}@example.net:8388/?plugin=kcptun%3Bmode%3Dfast")),
        LinkError::Unsupported(Unsupported::Plugin(p)) if p == "kcptun"
    ));
}

#[test]
fn socks_reads_both_shapes() {
    let login = |cfg: ServerConfig| match cfg.protocol {
        Protocol::Socks { login } => login.map(|l| (l.user.0, l.pass.0)),
        other => panic!("expected SOCKS, got {other:?}"),
    };
    let v2rayn = format!(
        "socks://{}@203.0.113.2:1080#s",
        URL_SAFE_NO_PAD.encode(format!("user:{PASSWORD}"))
    );
    assert_eq!(
        login(parse(&v2rayn)),
        Some(("user".to_owned(), PASSWORD.to_owned()))
    );
    assert_eq!(
        login(parse(&format!("socks5://user:{PASSWORD}@203.0.113.2:1080"))),
        Some(("user".to_owned(), PASSWORD.to_owned()))
    );
    assert_eq!(login(parse("socks://203.0.113.2:1080")), None);
}

#[test]
fn hysteria2_reads_hopping_ports_obfuscation_and_pins() {
    let pin = "AB:".repeat(31) + "AB";
    let cfg = parse(&format!(
        "hysteria2://{PASSWORD}@example.net:443,20000-30000/?sni=a.example\
         &obfs=salamander&obfs-password=OOOO&pinSHA256={pin}#hy"
    ));
    assert_eq!((cfg.port, cfg.tag.as_str()), (443, "hy"));
    assert!(matches!(&cfg.protocol, Protocol::Hysteria2 { auth } if auth.0 == PASSWORD));
    let Transport::Hysteria { obfs, hop } = &cfg.transport else {
        panic!("expected hysteria, got {:?}", cfg.transport);
    };
    assert_eq!(obfs.as_ref().map(|o| o.0.as_str()), Some("OOOO"));
    assert_eq!(hop.as_deref(), Some("443,20000-30000"));
    let Security::Tls(t) = &cfg.security else {
        panic!("Hysteria 2 is always TLS");
    };
    assert_eq!(t.sni.as_deref(), Some("a.example"));
    assert_eq!(t.pinned, [pin]);
    assert!(cfg.over_udp());

    // No port is 443; `hy2` is the short name, `mport` v2rayN's hop list.
    let cfg = parse(&format!(
        "hy2://{PASSWORD}@[2001:db8::1]/?insecure=1&mport=5000-6000"
    ));
    assert_eq!(cfg.port, 443);
    assert_eq!(cfg.address, Address::Ip("2001:db8::1".parse().unwrap()));
    assert!(matches!(&cfg.transport, Transport::Hysteria { hop: Some(h), .. } if h == "5000-6000"));
    assert!(matches!(&cfg.security, Security::Tls(t) if t.allow_insecure));

    assert!(matches!(
        refusal(&format!("hy2://{PASSWORD}@example.net/?obfs=gecko")),
        LinkError::Unsupported(Unsupported::Obfs(o)) if o == "gecko"
    ));
    assert!(matches!(
        refusal(&format!("hy2://{PASSWORD}@example.net/?obfs=salamander")),
        LinkError::Missing("obfs-password")
    ));
}

#[test]
fn wireguard_reads_keys_addresses_and_reserved_bytes() {
    let secret = ("A".repeat(43) + "=").replace('=', "%3D");
    let peer = "B".repeat(42) + "Q%3D";
    let cfg = parse(&format!(
        "wireguard://{secret}@203.0.113.8:2408?publickey={peer}\
         &address=172.16.0.2%2F32,2001:db8::2%2F128&reserved=1,2,3&mtu=1280#warp"
    ));
    let Protocol::WireGuard(wg) = &cfg.protocol else {
        panic!("expected WireGuard, got {:?}", cfg.protocol);
    };
    assert_eq!(wg.secret_key.0, [0; 32]);
    assert_eq!(wg.addresses, ["172.16.0.2/32", "2001:db8::2/128"]);
    assert_eq!(wg.reserved, Some([1, 2, 3]));
    assert_eq!(wg.mtu, Some(1280));
    assert!(wg.pre_shared_key.is_none());
    assert_eq!(cfg.transport, Transport::None);
    assert!(cfg.over_udp());

    // Reserved bytes as WARP's tools print them, in base64.
    let cfg = parse(&format!(
        "wg://{secret}@203.0.113.8:2408?publickey={peer}&reserved=AQID"
    ));
    assert!(matches!(&cfg.protocol, Protocol::WireGuard(wg) if wg.reserved == Some([1, 2, 3])));

    assert!(matches!(
        refusal(&format!("wireguard://{secret}@203.0.113.8:2408")),
        LinkError::Missing("publickey")
    ));
    assert!(matches!(
        refusal(&format!(
            "wireguard://{secret}@203.0.113.8:2408?publickey={peer}&reserved=1,2"
        )),
        LinkError::BadValue {
            param: "reserved",
            ..
        }
    ));
}

/// A link for a protocol xray lacks is told apart from a thing that is not a
/// server link at all, so the window can say which.
#[test]
fn protocols_xray_lacks_are_named_as_such() {
    for scheme in ["tuic", "anytls", "ssr", "hysteria"] {
        let err = refusal(&format!("{scheme}://secret@example.net:443"));
        assert!(
            matches!(&err, LinkError::NotInXray(s) if s == scheme),
            "{scheme}: got {err:?}"
        );
    }
    assert!(matches!(
        refusal("https://example.net/page"),
        LinkError::Scheme(s) if s == "https"
    ));
    assert!(bliss_proto::is_server_scheme("HY2"));
    assert!(!bliss_proto::is_server_scheme("tuic"));
}

/// `fm` and XHTTP's `extra` are passed to xray as written, so a broken one is
/// refused when the link is added rather than when xray starts.
#[test]
fn json_in_a_link_must_be_a_json_object() {
    const HEAD: &str = "vless://11111111-2222-3333-4444-555555555555@example.net:443?";
    for bad in ["fm=%5B1%5D", "fm=nope", "type=xhttp&extra=%7Bbroken"] {
        let err = refusal(&format!("{HEAD}{bad}"));
        assert!(
            matches!(err, LinkError::BadValue { .. }),
            "{bad}: got {err:?}"
        );
    }
    assert!(
        parse(&format!("{HEAD}fm=%7B%22udp%22%3A%5B%5D%7D"))
            .finalmask
            .is_some()
    );
}

#[test]
fn the_udp_servers_are_known_as_such() {
    assert!(!parse(PLAIN_TLS_LINK).over_udp());
    assert!(
        parse("vless://11111111-2222-3333-4444-555555555555@example.net:443?type=kcp").over_udp()
    );
}

/// The documented format has to be a format bliss reads.
///
/// `Profile` denies unknown fields, so a key misspelled in the example — or a
/// key documented there before it exists in the struct — would send anyone who
/// copies it straight into a refusal.
#[test]
fn the_example_profile_is_a_valid_profile() {
    const EXAMPLE: &str = include_str!("../../../bliss.example.toml");
    let profile: bliss_proto::Profile =
        toml::from_str(EXAMPLE).expect("the example profile must parse");

    // And the defaults it documents have to be the defaults it gets.
    assert_eq!(profile.inbound.socks, 10808);
    assert_eq!(profile.inbound.http, 10809);
    assert_eq!(profile.engine.bridge_port, 11080);
    assert_eq!(
        profile.inbound.system_proxy,
        bliss_proto::ProxyMode::Unchanged,
        "the one mode that cannot touch anything outside bliss"
    );
    assert!(
        !profile.tun.enabled,
        "the adapter is not created by a profile nobody has read yet"
    );
}

/// The rule sets' section is written only when it differs from what a profile
/// without it means, so a profile that never touched it stays as it was.
#[test]
fn the_rule_sets_section_is_written_only_when_it_says_something() {
    let profile = bliss_proto::Profile::default();
    assert!(profile.geo.auto && profile.geo.source.is_none());
    let text = toml::to_string(&profile).expect("serializes");
    assert!(!text.contains("[geo]"), "{text}");

    let mut changed = profile;
    changed.geo.source = Some("v2fly".to_owned());
    changed.geo.auto = false;
    let text = toml::to_string(&changed).expect("serializes");
    assert!(text.contains("[geo]"), "{text}");
    let back: bliss_proto::Profile = toml::from_str(&text).expect("reads back");
    assert_eq!(back.geo, changed.geo);
    assert!(toml::from_str::<bliss_proto::Profile>("[geo]\nsources = 1").is_err());
}

#[test]
fn short_id_may_be_absent_or_full_length() {
    assert!(ShortId::EMPTY.is_empty());
    assert_eq!(
        ShortId::parse_hex("f56336330b3feb01").unwrap().padded(),
        [0xf5, 0x63, 0x36, 0x33, 0x0b, 0x3f, 0xeb, 0x01]
    );
    // Odd length and over-long are both malformed rather than merely unusual.
    assert!(ShortId::parse_hex("abc").is_err());
    assert!(ShortId::parse_hex("f56336330b3feb0102").is_err());
}
