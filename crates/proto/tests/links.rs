//! Link parsing, checked against the shapes that actually turn up in the wild.
//!
//! Every link here is synthetic: same structure as a real one, TEST-NET address,
//! key material that is obviously fake. Real credentials belong in
//! `bliss.local.toml`, which is gitignored.

use bliss_proto::{
    Address, AppearanceMode, Encryption, EncryptionKey, Fingerprint, Flow, LinkError, RttMode,
    Security, ServerConfig, ShortId, TcpHeader, Transport,
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

fn reality(link: &str) -> ServerConfig {
    link.parse().expect("link should parse")
}

#[test]
fn parses_reality_with_vless_encryption() {
    let cfg = reality(REALITY_LINK);

    assert_eq!(cfg.tag, "test-reserved");
    assert_eq!(cfg.address, Address::Ip("203.0.113.10".parse().unwrap()));
    assert_eq!(cfg.port, 443);
    assert_eq!(cfg.flow, Flow::XtlsRprxVision);
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

    let Encryption::MlKem768X25519Plus(e) = &cfg.encryption else {
        panic!("expected the encryption layer, got {:?}", cfg.encryption);
    };
    assert_eq!(e.mode, AppearanceMode::Native);
    assert_eq!(e.rtt, RttMode::Zero);
    assert!(matches!(e.keys.as_slice(), [EncryptionKey::X25519(_)]));
}

#[test]
fn parsed_reality_link_matches_snapshot() {
    insta::assert_debug_snapshot!(reality(REALITY_LINK));
}

/// The whole point of the newtypes in `server.rs`. A ServerConfig reaches the
/// log pane and error reports; if this test ever fails, live credentials are
/// being written somewhere a user will paste into a bug report.
#[test]
fn debug_does_not_leak_credentials() {
    let rendered = format!("{:?}", reality(REALITY_LINK));

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

/// Stage 1 hands this string to xray-core verbatim, so it has to survive the
/// round trip byte for byte.
#[test]
fn encryption_parameter_round_trips() {
    let cfg = reality(REALITY_LINK);
    assert_eq!(
        cfg.encryption.as_param(),
        "mlkem768x25519plus.native.0rtt.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    );
    assert_eq!(reality(PLAIN_TLS_LINK).encryption.as_param(), "none");
}

#[test]
fn parses_plain_tls() {
    let cfg = reality(PLAIN_TLS_LINK);

    assert_eq!(cfg.address, Address::Domain("example.net".to_owned()));
    assert_eq!(cfg.flow, Flow::None);
    assert_eq!(cfg.encryption, Encryption::None);

    let Security::Tls(t) = &cfg.security else {
        panic!("expected tls, got {:?}", cfg.security);
    };
    assert_eq!(t.alpn, ["h2", "http/1.1"]);
    assert!(!t.allow_insecure);
    // No fp in the link: we parrot Chrome unless told otherwise.
    assert_eq!(t.fingerprint, Fingerprint::Chrome);
}

#[test]
fn tag_falls_back_to_address_when_the_fragment_is_missing() {
    let cfg =
        reality("vless://11111111-2222-3333-4444-555555555555@example.net:443?encryption=none");
    assert_eq!(cfg.tag, "example.net:443");
}

#[test]
fn rejects_transports_we_cannot_speak() {
    let err =
        "vless://11111111-2222-3333-4444-555555555555@example.net:443?encryption=none&type=ws"
            .parse::<ServerConfig>()
            .unwrap_err();
    assert!(
        matches!(&err, LinkError::UnsupportedTransport(t) if t == "ws"),
        "got {err:?}"
    );
}

#[test]
fn rejects_a_public_key_of_the_wrong_length() {
    let err = "vless://11111111-2222-3333-4444-555555555555@example.net:443\
        ?security=reality&sni=a.example&pbk=AAAA&sid=ab"
        .parse::<ServerConfig>()
        .unwrap_err();
    assert!(
        matches!(&err, LinkError::BadValue { param: "pbk", .. }),
        "got {err:?}"
    );
}

#[test]
fn rejects_reality_without_a_public_key() {
    let err = "vless://11111111-2222-3333-4444-555555555555@example.net:443\
        ?security=reality&sni=a.example"
        .parse::<ServerConfig>()
        .unwrap_err();
    assert!(
        matches!(err, LinkError::RealityMissing("pbk")),
        "got {err:?}"
    );
}

/// The file every new user is handed has to parse.
///
/// `Profile` denies unknown fields, so a key misspelled in the example — or a
/// key documented there before it exists in the struct — is a hard failure on
/// someone's very first run, at the moment they have least idea what is wrong.
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
