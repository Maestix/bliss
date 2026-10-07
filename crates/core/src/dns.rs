//! Asking a DNS server of our choosing, over a socket that leaves by an
//! interface of our choosing.
//!
//! One use: a site a rule sends through the VPN is looked up by the VPN's own
//! DNS server, through the VPN. A work network's names are known to that
//! server alone, and it should hear those names and no others — which is why
//! bliss keeps the server's DNS off the machine and asks it here instead.
//!
//! A records over UDP, which is all a dial needs; about a hundred lines where a
//! resolver crate would be thousands. An answer cut short is used for the
//! addresses it holds.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long to wait for one server's answer before asking the next.
const PATIENCE: Duration = Duration::from_secs(2);

/// Names remembered at once. A browser opening a work tool asks for the same
/// few names over and over; a cache that only grew would be the leak this
/// project exists to avoid.
const CACHE_CAP: usize = 256;

/// The longest an answer is trusted, whatever its TTL says.
const MAX_TTL: Duration = Duration::from_secs(300);

/// Looks names up with given servers, leaving by a given interface.
pub struct Resolver {
    servers: Vec<SocketAddr>,
    local: Ipv4Addr,
    index: u32,
    cache: Mutex<Cache>,
}

impl Resolver {
    /// `local` is the interface's own address, `index` its number.
    pub fn new(servers: Vec<SocketAddr>, local: Ipv4Addr, index: u32) -> Self {
        Self {
            servers,
            local,
            index,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Whether there is anyone to ask.
    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }

    /// Every IPv4 address `host` has, by the first server that answers.
    pub async fn lookup(&self, host: &str) -> io::Result<Vec<Ipv4Addr>> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(found) = self.cache.lock().expect("dns cache").get(&host) {
            return Ok(found);
        }
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no DNS server to ask");
        for server in &self.servers {
            match self.ask(*server, &host).await {
                Ok((addresses, ttl)) => {
                    self.cache
                        .lock()
                        .expect("dns cache")
                        .put(host.clone(), addresses.clone(), ttl);
                    return Ok(addresses);
                }
                // A name the server does not know is an answer: asking the
                // next server would only take longer to say the same thing.
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn ask(&self, server: SocketAddr, host: &str) -> io::Result<(Vec<Ipv4Addr>, Duration)> {
        let id = random_id();
        let packet = query(id, host)?;
        let socket = std::net::UdpSocket::bind(SocketAddr::from((self.local, 0)))?;
        socket.set_nonblocking(true)?;
        crate::interface::leave_by(&socket, self.index)?;
        let socket = tokio::net::UdpSocket::from_std(socket)?;
        // Connected, so an answer from anywhere but the server is dropped
        // before it is read.
        socket.connect(server).await?;
        socket.send(&packet).await?;

        let mut buf = [0u8; 1500];
        let wait = async {
            loop {
                let n = socket.recv(&mut buf).await?;
                match answers(id, &buf[..n]) {
                    // A stray reply to an earlier question: keep waiting.
                    Err(DnsError::OtherId) => continue,
                    result => return result.map_err(io::Error::from),
                }
            }
        };
        match tokio::time::timeout(PATIENCE, wait).await {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("DNS-сервер {server} не ответил за {} с", PATIENCE.as_secs()),
            )),
        }
    }
}

/// What can go wrong with a question or its answer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DnsError {
    #[error("«{0}» — не имя, которое можно спросить у DNS")]
    Name(String),
    #[error("DNS-сервер не знает такого имени")]
    NotFound,
    #[error("DNS-сервер ответил ошибкой {0}")]
    Server(u8),
    #[error("ответ DNS-сервера не разобрать")]
    Malformed,
    #[error("ответ на другой вопрос")]
    OtherId,
}

impl From<DnsError> for io::Error {
    fn from(error: DnsError) -> Self {
        let kind = match error {
            DnsError::NotFound => io::ErrorKind::NotFound,
            DnsError::Name(_) => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, error)
    }
}

/// The question: one name, type A, class IN, recursion wanted.
pub fn query(id: u16, host: &str) -> Result<Vec<u8>, DnsError> {
    let name = host.trim_end_matches('.');
    let bad = || DnsError::Name(name.to_owned());
    if name.is_empty() || name.len() > 253 || !name.is_ascii() {
        return Err(bad());
    }
    let mut packet = Vec::with_capacity(18 + name.len());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&[0x01, 0x00]); // a standard query, recursion desired
    packet.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // one question, nothing else
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(bad());
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&[0, 1, 0, 1]); // A, IN
    Ok(packet)
}

/// The IPv4 addresses in an answer to question `id`, and how long they may be
/// kept: the shortest TTL among them.
pub fn answers(id: u16, packet: &[u8]) -> Result<(Vec<Ipv4Addr>, Duration), DnsError> {
    let header = packet.get(..12).ok_or(DnsError::Malformed)?;
    if u16::from_be_bytes([header[0], header[1]]) != id {
        return Err(DnsError::OtherId);
    }
    if header[2] & 0x80 == 0 {
        // A question, not an answer.
        return Err(DnsError::Malformed);
    }
    match header[3] & 0x0f {
        0 => {}
        3 => return Err(DnsError::NotFound),
        code => return Err(DnsError::Server(code)),
    }
    let questions = u16::from_be_bytes([header[4], header[5]]);
    let records = u16::from_be_bytes([header[6], header[7]]);

    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(packet, at)? + 4;
    }
    let mut found = Vec::new();
    let mut ttl = MAX_TTL;
    for _ in 0..records {
        at = skip_name(packet, at)?;
        let fixed = packet.get(at..at + 10).ok_or(DnsError::Malformed)?;
        let kind = u16::from_be_bytes([fixed[0], fixed[1]]);
        let class = u16::from_be_bytes([fixed[2], fixed[3]]);
        let lives = u32::from_be_bytes([fixed[4], fixed[5], fixed[6], fixed[7]]);
        let length = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        at += 10;
        let data = packet.get(at..at + length).ok_or(DnsError::Malformed)?;
        at += length;
        // The CNAMEs a resolver sends along are passed over; the addresses
        // they lead to come after them.
        if kind == 1 && class == 1 && length == 4 {
            found.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
            ttl = ttl.min(Duration::from_secs(u64::from(lives)));
        }
    }
    if found.is_empty() {
        return Err(DnsError::NotFound);
    }
    Ok((found, ttl))
}

/// Past a name, which is labels ending in an empty one, or in a pointer to
/// a name earlier in the packet.
fn skip_name(packet: &[u8], mut at: usize) -> Result<usize, DnsError> {
    // A name has at most 127 labels; anything longer is not a name.
    for _ in 0..128 {
        let length = *packet.get(at).ok_or(DnsError::Malformed)?;
        match length {
            0 => return Ok(at + 1),
            l if l & 0xc0 == 0xc0 => {
                packet.get(at + 1).ok_or(DnsError::Malformed)?;
                return Ok(at + 2);
            }
            l if l & 0xc0 == 0 => at += 1 + usize::from(l),
            _ => return Err(DnsError::Malformed),
        }
    }
    Err(DnsError::Malformed)
}

/// A question's number: unpredictable enough that an answer to it cannot be
/// guessed, without a dependency for it.
fn random_id() -> u16 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    hasher.finish() as u16
}

/// Answers remembered for as long as their TTL allows, and no more of them
/// than `CACHE_CAP`.
#[derive(Default)]
struct Cache(HashMap<String, (Vec<Ipv4Addr>, Instant)>);

impl Cache {
    fn get(&self, host: &str) -> Option<Vec<Ipv4Addr>> {
        self.0
            .get(host)
            .filter(|(_, until)| *until > Instant::now())
            .map(|(addresses, _)| addresses.clone())
    }

    fn put(&mut self, host: String, addresses: Vec<Ipv4Addr>, ttl: Duration) {
        if ttl.is_zero() {
            return;
        }
        if self.0.len() >= CACHE_CAP {
            let now = Instant::now();
            self.0.retain(|_, (_, until)| *until > now);
        }
        if self.0.len() >= CACHE_CAP {
            // Every entry still fresh: start again rather than grow.
            self.0.clear();
        }
        self.0
            .insert(host, (addresses, Instant::now() + ttl.min(MAX_TTL)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The question as it goes on the wire, byte for byte.
    #[test]
    fn a_question_is_written_as_the_rfc_says() {
        let packet = query(0x1234, "tools.example.").expect("a valid name");
        assert_eq!(
            packet,
            [
                0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, // header
                5, b't', b'o', b'o', b'l', b's', 7, b'e', b'x', b'a', b'm', b'p', b'l', b'e', 0, 0,
                1, 0, 1, // A, IN
            ]
        );
        assert!(query(1, "a..example").is_err());
        assert!(query(1, "").is_err());
        assert!(
            query(1, "пример.example").is_err(),
            "names go out in punycode"
        );
        assert!(query(1, &format!("{}.example", "a".repeat(64))).is_err());
    }

    /// A reply the way resolvers send one: the question back, a CNAME, and
    /// the address it leads to, with the name compressed.
    fn reply(id: u16, code: u8) -> Vec<u8> {
        let mut packet = vec![
            (id >> 8) as u8,
            id as u8,
            0x81,
            0x80 | code,
            0,
            1,
            0,
            2,
            0,
            0,
            0,
            0,
        ];
        packet.extend_from_slice(&[5, b't', b'o', b'o', b'l', b's', 7]);
        packet.extend_from_slice(b"example");
        packet.extend_from_slice(&[0, 0, 1, 0, 1]);
        // tools.example CNAME web.example, TTL 600
        packet.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0x02, 0x58, 0, 6]);
        packet.extend_from_slice(&[3, b'w', b'e', b'b', 0xc0, 18]);
        // web.example A 10.20.30.40, TTL 60; its name is the CNAME's data,
        // at 43.
        packet.extend_from_slice(&[0xc0, 43, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 10, 20, 30, 40]);
        packet
    }

    #[test]
    fn an_answer_gives_its_addresses_and_the_shortest_life() {
        let (found, ttl) = answers(7, &reply(7, 0)).expect("it parses");
        assert_eq!(found, [Ipv4Addr::new(10, 20, 30, 40)]);
        assert_eq!(ttl, Duration::from_secs(60));
    }

    #[test]
    fn an_answer_is_matched_to_its_question() {
        assert_eq!(answers(8, &reply(7, 0)), Err(DnsError::OtherId));
        assert_eq!(answers(7, &reply(7, 3)), Err(DnsError::NotFound));
        assert_eq!(answers(7, &reply(7, 2)), Err(DnsError::Server(2)));
        let cut = reply(7, 0);
        assert_eq!(answers(7, &cut[..cut.len() - 3]), Err(DnsError::Malformed));
    }

    /// Past the cap the cache starts again rather than growing: the property
    /// that matters is that it cannot grow without bound.
    #[test]
    fn the_name_cache_has_a_ceiling() {
        let mut cache = Cache::default();
        for n in 0..CACHE_CAP * 3 {
            cache.put(
                format!("host{n}.example"),
                vec![Ipv4Addr::new(10, 0, 0, 1)],
                Duration::from_secs(60),
            );
            assert!(cache.0.len() <= CACHE_CAP);
        }
        assert!(
            cache
                .get(&format!("host{}.example", CACHE_CAP * 3 - 1))
                .is_some()
        );
    }

    #[test]
    fn an_answer_with_no_life_is_not_kept() {
        let mut cache = Cache::default();
        cache.put(
            "a.example".to_owned(),
            vec![Ipv4Addr::LOCALHOST],
            Duration::ZERO,
        );
        assert!(cache.get("a.example").is_none());
    }
}
