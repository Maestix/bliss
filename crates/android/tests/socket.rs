//! The engine's socket end to end, as the screens use it: hello, a link
//! pasted, deleted and given back, a rule switched. Everything but the TUN,
//! which only Android has.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::Value;

fn wait_for(
    lines: &mut impl Iterator<Item = Value>,
    what: &str,
    ok: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    for frame in lines {
        if ok(&frame) {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame with {what}");
    }
    panic!("the socket closed before {what}");
}

#[test]
fn the_screens_drive_the_engine_over_its_socket() {
    let dir = std::env::temp_dir().join(format!("bliss-socket-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("engine.sock");
    bliss::engine::start(bliss::engine::Paths {
        files: dir.clone(),
        libs: dir.clone(),
        socket: socket.clone(),
    })
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => panic!("no socket: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut out = stream.try_clone().unwrap();
    let mut send = |line: &str| {
        out.write_all(line.as_bytes()).unwrap();
        out.write_all(b"\n").unwrap();
    };
    // Each line holds the parts that changed; the screens merge them into
    // what they have, and so does this.
    let mut whole = serde_json::Map::new();
    let mut frames = BufReader::new(stream).lines().map(|line| {
        let Value::Object(patch) = serde_json::from_str::<Value>(&line.unwrap()).unwrap() else {
            panic!("a line that is not an object");
        };
        whole.extend(patch);
        Value::Object(whole.clone())
    });

    send(r#"{"cmd":"hello","full":true}"#);
    let first = wait_for(&mut frames, "the first frame", |f| f["state"] == "off");
    assert_eq!(first["rules"][0]["name"], "Локальная сеть");

    let link = "vless://11111111-2222-3333-4444-555555555555@203.0.113.7:443?security=reality&sni=www.example.net&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&fp=chrome&type=tcp#Test";
    send(&serde_json::json!({"cmd": "import", "text": link}).to_string());
    let added = wait_for(&mut frames, "the server", |f| {
        f["servers"].as_array().is_some_and(|s| s.len() == 1)
    });
    assert_eq!(added["servers"][0]["name"], "Test");
    assert_eq!(added["servers"][0]["kind"], "VLESS · REALITY");
    assert_eq!(added["current"], 0);
    assert!(
        !added.to_string().contains("11111111"),
        "a frame carried the link"
    );

    send(r#"{"cmd":"delete","index":0}"#);
    let gone = wait_for(&mut frames, "the deletion", |f| {
        f["servers"].as_array().is_some_and(|s| s.is_empty())
    });
    assert_eq!(gone["toast"]["undo"], true);
    assert_eq!(gone["can_restore"], true);

    send(r#"{"cmd":"restore"}"#);
    wait_for(&mut frames, "the server back", |f| {
        f["servers"].as_array().is_some_and(|s| s.len() == 1)
    });

    send(r#"{"cmd":"rule","index":0,"on":false}"#);
    wait_for(&mut frames, "the rule off", |f| {
        f["rules"][0]["enabled"] == false
    });

    // A line with no end is not read for ever: past 4 MB the socket closes.
    let mut endless = UnixStream::connect(&socket).unwrap();
    endless
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let block = vec![b'x'; 1 << 20];
    for _ in 0..5 {
        if endless.write_all(&block).is_err() {
            break;
        }
    }
    // Frames keep coming while the socket is open, so read until the end
    // of the stream, or its reset (the engine left bytes unread), for ten
    // seconds at most.
    use std::io::Read;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut chunk = [0u8; 64 << 10];
    loop {
        match endless.read(&mut chunk) {
            Ok(0) => break,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
            Ok(_) | Err(_) => {}
        }
        assert!(Instant::now() < deadline, "the engine kept the socket open");
    }

    // And it all reached the profile on disk.
    let profile = std::fs::read_to_string(dir.join("bliss.toml")).unwrap();
    assert!(profile.contains("enabled = false"), "{profile}");
    assert!(profile.contains("203.0.113.7"), "{profile}");
    let _ = std::fs::remove_dir_all(&dir);
}
