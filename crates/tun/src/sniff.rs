//! A site's name read out of the connection itself.
//!
//! Where a program found its address some way bliss did not answer — Android's
//! «Частный DNS» set to a server's name, a browser's own DNS over HTTPS, an
//! address it kept — a TUN connection arrives as a bare address, and `domain`
//! and `geosite` rules cannot see it. Its first bytes still say: a TLS
//! ClientHello names the site in its `server_name` extension, a plain HTTP
//! request in its `Host:` header. Only what is read here is used to decide;
//! the bytes themselves go on to the far end unchanged.

/// What the first bytes say, so far.
#[derive(Debug, PartialEq, Eq)]
pub enum Sniffed {
    /// The site's name.
    Name(String),
    /// A ClientHello or a request head, not all here yet: read more.
    More,
    /// Neither, or one that names no site.
    Nothing,
}

/// The most read while looking for a name: a ClientHello with a post-quantum
/// key share is some 1.8 KB, and a request head rarely past 4 KB.
pub const MAX_SNIFF: usize = 16 * 1024;

/// Read a name out of the first bytes of a connection.
pub fn name(bytes: &[u8]) -> Sniffed {
    match bytes.first() {
        None => Sniffed::More,
        Some(0x16) => tls_server_name(bytes),
        Some(b) if b.is_ascii_uppercase() => http_host(bytes),
        Some(_) => Sniffed::Nothing,
    }
}

fn tls_server_name(bytes: &[u8]) -> Sniffed {
    if bytes.len() < 5 {
        return Sniffed::More;
    }
    let record = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    if 5 + record > MAX_SNIFF {
        return Sniffed::Nothing;
    }
    if bytes.len() < 5 + record {
        return Sniffed::More;
    }
    let hello = &bytes[5..5 + record];
    match read_server_name(hello) {
        Some(name) => Sniffed::Name(name),
        None => Sniffed::Nothing,
    }
}

/// The `server_name` of a ClientHello held whole in one record.
fn read_server_name(hello: &[u8]) -> Option<String> {
    if *hello.first()? != 0x01 {
        return None;
    }
    let mut at = 4 + 2 + 32; // handshake header, version, random
    let session = *hello.get(at)? as usize;
    at += 1 + session;
    let suites = u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]) as usize;
    at += 2 + suites;
    let compression = *hello.get(at)? as usize;
    at += 1 + compression;
    let extensions = u16::from_be_bytes([*hello.get(at)?, *hello.get(at + 1)?]) as usize;
    at += 2;
    let end = (at + extensions).min(hello.len());
    while at + 4 <= end {
        let kind = u16::from_be_bytes([hello[at], hello[at + 1]]);
        let len = u16::from_be_bytes([hello[at + 2], hello[at + 3]]) as usize;
        let body = hello.get(at + 4..at + 4 + len)?;
        if kind == 0 {
            // A list of names, of which only the host name kind exists.
            let mut i = 2;
            while i + 3 <= body.len() {
                let kind = body[i];
                let n = u16::from_be_bytes([body[i + 1], body[i + 2]]) as usize;
                let name = body.get(i + 3..i + 3 + n)?;
                if kind == 0 {
                    return host(name);
                }
                i += 3 + n;
            }
            return None;
        }
        at += 4 + len;
    }
    None
}

fn http_host(bytes: &[u8]) -> Sniffed {
    // A request line starts with a method: letters, then a space.
    let method = bytes.iter().take_while(|b| b.is_ascii_uppercase()).count();
    match bytes.get(method) {
        None if method < 8 => return Sniffed::More,
        Some(b' ') if method >= 3 => {}
        _ => return Sniffed::Nothing,
    }
    let head_end = bytes.windows(4).position(|w| w == b"\r\n\r\n");
    let head = &bytes[..head_end.unwrap_or(bytes.len())];
    for line in head.split(|&b| b == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > 5 && line[..5].eq_ignore_ascii_case(b"host:") {
            let value = trim(&line[5..]);
            // Without the port, and without an IPv6 literal's brackets.
            let value = match value.iter().rposition(|&b| b == b':') {
                Some(colon)
                    if colon > 0 && (!value.contains(&b']') || value[colon - 1] == b']') =>
                {
                    &value[..colon]
                }
                _ => value,
            };
            return match host(value) {
                Some(name) => Sniffed::Name(name),
                None => Sniffed::Nothing,
            };
        }
    }
    if head_end.is_some() || bytes.len() >= MAX_SNIFF {
        Sniffed::Nothing
    } else {
        Sniffed::More
    }
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

/// A name as rules compare it, or none for one that is not a host name —
/// an address written as text is left to the address it already is.
fn host(raw: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(raw)
        .ok()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let valid = !name.is_empty()
        && name.len() <= 253
        && name.contains('.')
        && name.parse::<std::net::IpAddr>().is_err()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_');
    valid.then_some(name)
}

/// Whether a datagram opens a QUIC connection: a long header, the Initial
/// type, version 1 or 2. Its name is encrypted (with a key anyone can derive,
/// but not read here), so a QUIC connection to a bare address is refused and
/// the program falls back to TCP, whose name `name` reads.
pub fn is_quic_initial(datagram: &[u8]) -> bool {
    let Some(&first) = datagram.first() else {
        return false;
    };
    if first & 0xc0 != 0xc0 || datagram.len() < 1200 {
        return false;
    }
    let version = u32::from_be_bytes([datagram[1], datagram[2], datagram[3], datagram[4]]);
    match version {
        0x0000_0001 => (first >> 4) & 0x03 == 0,
        // QUIC v2 numbers its long header types otherwise: Initial is 1.
        0x6b33_43cf => (first >> 4) & 0x03 == 1,
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A ClientHello naming `name`, with other extensions around it, as one
    /// record: the shape every browser sends.
    pub(crate) fn client_hello(name: &str) -> Vec<u8> {
        let mut sni = vec![0, 0];
        let list_len = (3 + name.len()) as u16;
        let mut list = list_len.to_be_bytes().to_vec();
        list.push(0);
        list.extend_from_slice(&(name.len() as u16).to_be_bytes());
        list.extend_from_slice(name.as_bytes());
        sni.extend_from_slice(&(list.len() as u16).to_be_bytes());
        sni.extend_from_slice(&list);
        // GREASE before it and ALPN after, as Chrome shuffles them.
        let mut exts = vec![0x0a, 0x0a, 0, 0];
        exts.extend_from_slice(&sni);
        exts.extend_from_slice(&[0, 16, 0, 5, 0, 3, 2, b'h', b'2']);
        let mut body = vec![3, 3];
        body.extend_from_slice(&[0x11; 32]);
        body.push(32);
        body.extend_from_slice(&[0x22; 32]);
        body.extend_from_slice(&[0, 2, 0x13, 0x01, 1, 0]);
        body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        body.extend_from_slice(&exts);
        let mut hs = vec![1, 0, (body.len() >> 8) as u8, body.len() as u8];
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 3, 1];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn a_client_hello_names_its_site() {
        let hello = client_hello("WWW.Reddit.com");
        assert_eq!(name(&hello), Sniffed::Name("www.reddit.com".into()));
        // Cut anywhere before its end, it asks for more.
        for cut in [1, 4, 5, 40, hello.len() - 1] {
            assert_eq!(name(&hello[..cut]), Sniffed::More, "cut at {cut}");
        }
        // An address in place of a name is no name.
        assert_eq!(name(&client_hello("203.0.113.5")), Sniffed::Nothing);
    }

    #[test]
    fn a_request_names_its_host() {
        let req = b"GET / HTTP/1.1\r\nUser-Agent: x\r\nhOsT:  old.Reddit.com:8080 \r\n\r\n";
        assert_eq!(name(req), Sniffed::Name("old.reddit.com".into()));
        assert_eq!(name(b"GET / HTTP/1.1\r\nUser-Ag"), Sniffed::More);
        assert_eq!(name(b"GE"), Sniffed::More);
        assert_eq!(name(b"GET / HTTP/1.1\r\n\r\n"), Sniffed::Nothing);
        // SSH and anything binary are not asked about.
        assert_eq!(name(b"SSH-2.0-OpenSSH_9.6\r\n"), Sniffed::Nothing);
        assert_eq!(name(&[0x00, 0x01, 0x02]), Sniffed::Nothing);
    }

    #[test]
    fn a_quic_initial_is_told_from_other_datagrams() {
        let mut initial = vec![0u8; 1200];
        initial[0] = 0xc3;
        initial[1..5].copy_from_slice(&1u32.to_be_bytes());
        assert!(is_quic_initial(&initial));
        // A handshake packet, a short header, a datagram too small for an
        // Initial, and something that is not QUIC at all.
        let mut handshake = initial.clone();
        handshake[0] = 0xe3;
        assert!(!is_quic_initial(&handshake));
        let mut short = initial.clone();
        short[0] = 0x43;
        assert!(!is_quic_initial(&short));
        assert!(!is_quic_initial(&initial[..100]));
        assert!(!is_quic_initial(&[0x17; 1300]));
    }
}
